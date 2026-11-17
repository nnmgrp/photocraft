//! Actions set & step editing commands missing from upstream 0.23:
//! `actions.renameSet`, `actions.deleteSet`, `actions.clear`, `actions.setStep`, `actions.options`.

use serde_json::{Value, json};

use crate::actions_cmds::{Action, PLAY_SHORTCUT_PREFIX, flush_recording, not_playing, note_automation_edit, pending_steps, resolve};
use crate::actions_edit_cmds::set_names;
use crate::commands::CommandSpec;
use crate::prefs::{Actions as ActionsPrefs, PlaybackPerformance};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

fn req_set_name(p: &Value, key: &str, cmd: &str) -> Result<String> {
    match p.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(Value::String(_)) => Err(bad(cmd, format!("`{key}` is empty"))),
        Some(_) => Err(bad(cmd, format!("`{key}` must be a string"))),
        None => Err(bad(cmd, format!("missing `{key}`"))),
    }
}

fn rename_set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "actions.renameSet";
    not_playing(s, cmd)?;
    let set = req_set_name(p, "set", cmd)?;
    let name = req_set_name(p, "name", cmd)?;
    let names = set_names(s);
    if !names.contains(&set) {
        return Err(bad(cmd, format!("there is no set named `{set}`")));
    }
    if name != set && names.contains(&name) {
        return Err(bad(cmd, format!("a set named `{name}` already exists")));
    }
    for action in &mut s.actions.list {
        if action.set.as_deref() == Some(&set) {
            action.set = Some(name.clone());
        }
    }
    for empty in &mut s.actions.sets {
        if empty == &set {
            *empty = name.clone();
        }
    }
    s.actions.touch();
    Ok(json!({ "set": name }))
}

fn remap_calls_on_delete(list: &mut [Action], removed_indices: &[usize]) {
    if removed_indices.is_empty() {
        return;
    }
    for action in list {
        for (id, params) in &mut action.steps {
            if !matches!(id.as_str(), "actions.play" | "file.automate.batch") {
                continue;
            }
            let Some(old) = params.get("action").and_then(Value::as_u64).map(|n| n as usize) else {
                continue;
            };
            let removed_before = removed_indices.iter().filter(|&&i| i < old).count();
            if removed_before > 0 {
                params["action"] = json!(old.saturating_sub(removed_before));
            }
        }
    }
}

fn delete_set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "actions.deleteSet";
    not_playing(s, cmd)?;
    let set = req_set_name(p, "set", cmd)?;
    if !set_names(s).contains(&set) {
        return Err(bad(cmd, format!("there is no set named `{set}`")));
    }
    if let Some((_, rec_idx)) = s.actions.recording
        && s.actions.list.get(rec_idx).is_some_and(|a| a.set.as_deref() == Some(&set))
    {
        return Err(bad(cmd, "stop recording before deleting the set being recorded into"));
    }

    let mut removed_indices = Vec::new();
    let mut removed_names = Vec::new();
    let mut kept = Vec::with_capacity(s.actions.list.len());
    for (i, action) in s.actions.list.drain(..).enumerate() {
        if action.set.as_deref() == Some(&set) {
            removed_indices.push(i);
            removed_names.push(action.name);
        } else {
            kept.push(action);
        }
    }
    s.actions.list = kept;
    remap_calls_on_delete(&mut s.actions.list, &removed_indices);
    if let Some((_, rec_idx)) = s.actions.recording.as_mut() {
        let shift = removed_indices.iter().filter(|&&i| i < *rec_idx).count();
        *rec_idx = rec_idx.saturating_sub(shift);
    }
    s.actions.sets.retain(|n| n != &set);
    s.actions.touch();

    let keys: Vec<String> = removed_names.into_iter().map(|n| format!("{PLAY_SHORTCUT_PREFIX}{n}")).collect();
    if keys.iter().any(|k| s.prefs().shortcuts.contains_key(k)) {
        s.edit_prefs(|prefs| {
            for k in &keys {
                prefs.shortcuts.remove(k);
            }
        });
    }
    Ok(json!({ "set": set, "actions": removed_indices.len() }))
}

fn clear_all(s: &mut Session, _: &Value) -> Result<Value> {
    let cmd = "actions.clear";
    not_playing(s, cmd)?;
    if s.actions.recording.is_some() {
        return Err(bad(cmd, "stop recording before clearing all actions"));
    }
    let deleted = s.actions.list.len();
    let keys: Vec<String> = s.actions.list.iter().map(|a| format!("{PLAY_SHORTCUT_PREFIX}{}", a.name)).collect();
    s.actions.list.clear();
    s.actions.sets.clear();
    s.actions.touch();
    if keys.iter().any(|k| s.prefs().shortcuts.contains_key(k)) {
        s.edit_prefs(|prefs| {
            for k in &keys {
                prefs.shortcuts.remove(k);
            }
        });
    }
    Ok(json!({ "deleted": deleted }))
}

fn set_step(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "actions.setStep";
    not_playing(s, cmd)?;
    let idx = resolve(&s.actions.list, p, cmd)?;
    let step = p.get("step").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(cmd, "`step` must be a non-negative integer"))?;
    let params = p.get("params").filter(|v| v.is_object()).cloned().ok_or_else(|| bad(cmd, "`params` must be a JSON object"))?;
    let len = s.actions.list[idx].steps.len().saturating_add(pending_steps(s, idx).count());
    if step >= len {
        return Err(bad(cmd, "step is out of range"));
    }
    if s.actions.recording.is_some_and(|(_, i)| i == idx) {
        flush_recording(s)?;
    }
    let action = &mut s.actions.list[idx];
    let step_id = action.steps[step].0.clone();
    action.steps[step].1 = params.clone();
    let name = action.name.clone();
    note_automation_edit(s, idx);
    s.actions.touch();
    Ok(json!({ "action": name, "step": step, "id": step_id, "params": params }))
}

fn playback_json(prefs: &ActionsPrefs) -> Value {
    match prefs.performance {
        PlaybackPerformance::Accelerated => json!({ "mode": "accelerated" }),
        PlaybackPerformance::StepByStep => json!({ "mode": "stepByStep" }),
        PlaybackPerformance::Pause => json!({ "mode": "pause", "seconds": f64::from(prefs.pause_seconds()) }),
    }
}

fn parse_playback(v: &Value, current_seconds: u8, cmd: &str) -> Result<(PlaybackPerformance, Option<u8>)> {
    let Some(obj) = v.as_object() else {
        return Err(bad(cmd, "`playback` must be an object"));
    };
    let mode = match obj.get("mode").and_then(Value::as_str) {
        Some("accelerated") => PlaybackPerformance::Accelerated,
        Some("stepByStep" | "step") => PlaybackPerformance::StepByStep,
        Some("pause") => PlaybackPerformance::Pause,
        _ => return Err(bad(cmd, "`playback.mode` must be accelerated, stepByStep or pause")),
    };
    let seconds = match obj.get("seconds") {
        None | Some(Value::Null) => (mode == PlaybackPerformance::Pause).then_some(current_seconds),
        Some(n) => {
            let secs = n
                .as_f64()
                .filter(|s| s.is_finite())
                .map(|s| s.round())
                .filter(|&s| (f64::from(*ActionsPrefs::PAUSE_SECONDS.start())..=f64::from(*ActionsPrefs::PAUSE_SECONDS.end())).contains(&s))
                .map(|s| s as u8)
                .ok_or_else(|| bad(cmd, "`playback.seconds` must be between 1 and 60"))?;
            Some(secs)
        }
    };
    Ok((mode, seconds))
}

fn options(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "actions.options";
    not_playing(s, cmd)?;
    if !p.is_object() {
        return Err(bad(cmd, "params must be an object"));
    }
    let allow_tool_recording = match p.get("allowToolRecording") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(bad(cmd, "`allowToolRecording` must be true or false")),
    };
    let current_secs = s.prefs().actions.pause_seconds();
    let playback = match p.get("playback") {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_playback(v, current_secs, cmd)?),
    };
    if let Some(enabled) = allow_tool_recording
        && enabled != s.actions.record_tools
    {
        if s.actions.recording.is_some() && s.tool_stroke_job_running() {
            return Err(bad(cmd, "wait for the current tool stroke before changing recording"));
        }
        flush_recording(s)?;
        s.actions.record_tools = enabled;
    }
    if let Some((mode, secs)) = playback {
        s.edit_prefs(|prefs| {
            prefs.actions.performance = mode;
            if let Some(secs) = secs {
                prefs.actions.pause_seconds = secs;
            }
        });
    }
    Ok(json!({
        "allowToolRecording": s.actions.record_tools,
        "playback": playback_json(&s.prefs().actions),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "actions.renameSet",
            label: "Rename Action Set",
            menu: &[],
            shortcut: None,
            params: r##"{"set":name,"name":str} -> {set}"##,
            enabled: always,
            run: rename_set,
            journal: false,
        },
        CommandSpec {
            id: "actions.deleteSet",
            label: "Delete Action Set",
            menu: &[],
            shortcut: None,
            params: r##"{"set":name} -> {set,actions}"##,
            enabled: always,
            run: delete_set,
            journal: false,
        },
        CommandSpec {
            id: "actions.clear",
            label: "Clear All Actions",
            menu: &[],
            shortcut: None,
            params: r##"{} -> {deleted}"##,
            enabled: always,
            run: clear_all,
            journal: false,
        },
        CommandSpec {
            id: "actions.setStep",
            label: "Set Action Step Parameters",
            menu: &[],
            shortcut: None,
            params: r##"{"action":name|index,"step":index,"params":{}} -> {action,step,id,params}"##,
            enabled: always,
            run: set_step,
            journal: false,
        },
        CommandSpec {
            id: "actions.options",
            label: "Actions Options",
            menu: &[],
            shortcut: None,
            params: r##"{"allowToolRecording":bool?,"playback":{"mode":"accelerated"|"stepByStep"|"pause","seconds":1..=60?}?} -> {allowToolRecording,playback}"##,
            enabled: always,
            run: options,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::atn_cmds::DEFAULT_SET;

    fn names(s: &Session) -> Vec<(String, Option<String>)> {
        s.actions.list.iter().map(|a| (a.name.clone(), a.set.clone())).collect()
    }

    fn layer(name: &str) -> (String, Value) {
        ("layer.new.layer".to_string(), json!({ "name": name }))
    }

    fn step_names(s: &Session, action: usize) -> Vec<String> {
        s.actions.list[action].steps.iter().map(|(_, p)| p["name"].as_str().unwrap_or("").to_string()).collect()
    }

    #[test]
    fn sets_are_made_renamed_and_deleted_with_their_actions() {
        let mut s = Session::new();
        assert_eq!(s.execute("actions.newSet", json!({})).unwrap()["set"], "Set 1");
        assert_eq!(s.execute("actions.newSet", json!({})).unwrap()["set"], "Set 2");
        assert_eq!(set_names(&s), ["Set 1", "Set 2"]);
        s.execute("actions.record", json!({"name": "A", "set": "Set 2"})).unwrap();
        s.execute("actions.stop", json!({})).unwrap();
        s.execute("actions.record", json!({"name": "B", "set": "Set 1"})).unwrap();
        s.execute("actions.stop", json!({})).unwrap();
        s.execute("actions.record", json!({"name": "C", "set": "Set 2"})).unwrap();
        s.execute("actions.stop", json!({})).unwrap();
        s.execute("actions.renameSet", json!({"set": "Set 2", "name": "Mine"})).unwrap();
        assert_eq!(s.actions.list[0].set.as_deref(), Some("Mine"));
        let r = s.execute("actions.deleteSet", json!({"set": "Mine"})).unwrap();
        assert_eq!(r["actions"], 2);
        assert_eq!(names(&s), [("B".into(), Some("Set 1".into()))]);
        assert_eq!(set_names(&s), ["Set 1"]);
        s.actions.list.push(Action { set: Some("Loaded".into()), ..Action::new("D", vec![]) });
        for bad_req in [json!({}), json!({"set": "Nope"}), json!({"set": ""}), json!({"set": 3})] {
            assert!(s.execute("actions.deleteSet", bad_req.clone()).is_err(), "{bad_req}");
            assert!(s.execute("actions.renameSet", bad_req.clone()).is_err(), "{bad_req}");
        }
        assert!(s.execute("actions.renameSet", json!({"set": "Set 1", "name": "Loaded"})).is_err());
    }

    #[test]
    fn deleting_an_earlier_set_keeps_the_recording_attached() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        s.actions.list.push(Action { set: Some("Old".into()), ..Action::new("Gone", vec![]) });
        s.execute("actions.record", json!({"name": "Live", "set": DEFAULT_SET})).unwrap();
        assert!(s.execute("actions.deleteSet", json!({"set": DEFAULT_SET})).is_err());
        s.execute("actions.deleteSet", json!({"set": "Old"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "X"})).unwrap();
        s.execute("actions.stop", json!({})).unwrap();
        assert_eq!(step_names(&s, 0), ["X"]);
    }

    #[test]
    fn clear_all_actions_empties_the_panel() {
        let mut s = Session::new();
        s.execute("actions.newSet", json!({})).unwrap();
        s.actions.list.push(Action::new("A", vec![]));
        s.execute("actions.record", json!({"action": 0})).unwrap();
        assert!(s.execute("actions.clear", json!({})).is_err());
        s.execute("actions.stop", json!({})).unwrap();
        assert_eq!(s.execute("actions.clear", json!({})).unwrap()["deleted"], 1);
        assert!(s.actions.list.is_empty() && set_names(&s).is_empty());
    }

    #[test]
    fn set_step_replaces_the_values() {
        let mut s = Session::new();
        s.actions.list.push(Action::new("A", vec![layer("Old")]));
        s.execute("actions.setStep", json!({"action": 0, "step": 0, "params": {"name": "New"}})).unwrap();
        assert_eq!(s.actions.list[0].steps, [layer("New")]);
        for bad_req in [
            json!({"action": 0, "step": 1, "params": {}}),
            json!({"action": 0, "step": 0}),
            json!({"action": 0, "params": {}}),
            json!({"action": 0, "step": 0, "params": 3}),
        ] {
            assert!(s.execute("actions.setStep", bad_req.clone()).is_err(), "{bad_req}");
        }
    }

    #[test]
    fn options_set_tool_recording_and_playback() {
        let mut s = Session::new();
        let r = s.execute("actions.options", json!({})).unwrap();
        assert_eq!(r, json!({"allowToolRecording": false, "playback": {"mode": "accelerated"}}));
        s.execute("actions.options", json!({"allowToolRecording": true, "playback": {"mode": "pause", "seconds": 2}})).unwrap();
        assert!(s.actions.record_tools);
        assert_eq!(s.prefs().actions.performance, PlaybackPerformance::Pause);
        assert_eq!(s.prefs().actions.pause_seconds(), 2);
        s.execute("actions.options", json!({"playback": {"mode": "stepByStep"}})).unwrap();
        assert!(s.actions.record_tools, "omitted keys stay");
        for bad_req in [
            json!({"allowToolRecording": "yes"}),
            json!({"playback": "fast"}),
            json!({"playback": {"mode": "pause", "seconds": -1}}),
            json!({"playback": {"mode": "pause", "seconds": 1000}}),
            json!({"playback": {"mode": "warp"}}),
        ] {
            assert!(s.execute("actions.options", bad_req.clone()).is_err(), "{bad_req}");
        }
        assert_eq!(s.prefs().actions.performance, PlaybackPerformance::StepByStep);
    }
}
