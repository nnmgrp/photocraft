use serde_json::json;

use super::*;
use photocraft_doc::{Document, LayerContent, LayerId};

fn session(w: u32, h: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "background": "white", "depth": 8})).unwrap();
    s
}

fn doc(s: &Session) -> &Document {
    &s.active().unwrap().doc
}

fn shape(s: &Session, id: u64) -> ShapeLayer {
    match &doc(s).layer(LayerId(id)).unwrap().content {
        LayerContent::Shape(sh) => sh.clone(),
        other => panic!("not a shape: {}", other.kind_name()),
    }
}

fn ellipse_layer(s: &mut Session) -> u64 {
    s.execute("shape.create", json!({"kind": "ellipse", "rect": [20, 20, 100, 80]})).unwrap()["layer"].as_u64().unwrap()
}

#[test]
fn a_tail_is_unioned_into_one_contour_and_undoes_in_one_step() {
    let mut s = session(200, 200);
    let id = ellipse_layer(&mut s);
    let before = shape(&s, id).path.clone();
    let history = s.active().unwrap().history.past_len();

    let info = s.execute("shape.balloon.tail", json!({"layer": id, "preset": "wedge", "tip": [90, 160]})).unwrap();
    assert_eq!(info["layer"].as_u64(), Some(id));
    let merged = shape(&s, id);
    assert_eq!(merged.path.subpaths.len(), 1, "body and tail share one contour");
    assert!(merged.path.subpaths[0].closed);
    assert_ne!(merged.path, before);
    assert_eq!(s.active().unwrap().history.past_len(), history + 1, "one undoable step");

    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(shape(&s, id).path, before);
}

#[test]
fn every_tail_preset_and_thought_bubbles_apply() {
    for preset in ["wedge", "arc", "double", "thought"] {
        let mut s = session(200, 200);
        let id = ellipse_layer(&mut s);
        s.execute("shape.balloon.tail", json!({"layer": id, "preset": preset, "tip": [100, 180], "bubbles": 3, "bubbleRadius": 5})).unwrap();
        let sh = shape(&s, id);
        assert!(!sh.path.subpaths.is_empty(), "{preset}");
        if preset == "thought" {
            assert!(sh.path.subpaths.len() >= 2, "thought bubbles are separate contours");
        } else {
            assert_eq!(sh.path.subpaths.len(), 1, "{preset} merges with the body");
        }
    }
}

#[test]
fn a_tail_can_be_dragged_from_a_point_instead_of_a_base() {
    let mut s = session(200, 200);
    let id = ellipse_layer(&mut s);
    s.execute("shape.balloon.tail", json!({"layer": id, "from": [70, 100], "tip": [150, 180], "mouthWidth": 20})).unwrap();
    assert_eq!(shape(&s, id).path.subpaths.len(), 1);
}

#[test]
fn cloud_and_spiky_balloons_become_shape_layers() {
    let mut s = session(300, 300);
    let cloud = s.execute("shape.balloon.create", json!({"kind": "cloud", "rect": [20, 20, 200, 120], "bumps": 11})).unwrap();
    assert!(cloud["name"].as_str().is_some_and(|n| n.starts_with("Thought Cloud")), "{}", cloud["name"]);
    let cloud_id = cloud["layer"].as_u64().unwrap();
    assert!(shape(&s, cloud_id).path.subpaths[0].closed);

    let spiky = s.execute("shape.balloon.create", json!({"kind": "spiky", "rect": [20, 160, 200, 120], "spikes": 14})).unwrap();
    assert!(spiky["name"].as_str().is_some_and(|n| n.starts_with("Shout Balloon")), "{}", spiky["name"]);
    let spiky_id = spiky["layer"].as_u64().unwrap();
    assert_eq!(shape(&s, spiky_id).path.subpaths[0].knots.len(), 28, "one valley and one tip per spike");

    // Both are ordinary documents content: undo removes each layer in turn.
    s.execute("edit.undo", json!({})).unwrap();
    assert!(doc(&s).layer(LayerId(spiky_id)).is_none());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(doc(&s).layer(LayerId(cloud_id)).is_none());
}

#[test]
fn a_sai_stroke_becomes_a_tapered_outline_or_a_uniform_stroke() {
    let mut s = session(300, 200);
    let points = json!([{"x": 10.0, "y": 100.0, "pressure": 0.0}, {"x": 60.0, "y": 60.0, "pressure": 1.0}, {"x": 140.0, "y": 100.0, "pressure": 0.0}]);
    let tapered = s.execute("shape.balloon.stroke", json!({"points": points, "width": 12.0})).unwrap();
    let id = tapered["layer"].as_u64().unwrap();
    let sh = shape(&s, id);
    assert!(sh.fill.is_some(), "tapered ink is a filled outline");
    assert!(sh.stroke.is_none());
    assert_eq!(sh.path.subpaths.len(), 1);

    let uniform = s.execute("shape.balloon.stroke", json!({"points": points, "width": 4.0, "tapered": false})).unwrap();
    let uid = uniform["layer"].as_u64().unwrap();
    let sh = shape(&s, uid);
    assert!(sh.fill.is_none(), "a uniform stroke path is stroke only");
    assert_eq!(sh.stroke.as_ref().map(|s| s.width), Some(4.0));
}

#[test]
fn hostile_or_missing_params_are_errors_without_editing_the_document() {
    let mut s = session(200, 200);
    let id = ellipse_layer(&mut s);
    let before = doc(&s).clone();
    let history = s.active().unwrap().history.past_len();
    let cases = [
        json!({"layer": id, "preset": "nope", "tip": [10, 10]}),
        json!({"layer": id}),
        json!({"layer": id, "tip": [1e300, 0]}),
        json!({"layer": id, "tip": [10, 10], "from": [10, 10]}),
        json!({"layer": id, "tip": [10, 10], "base": [[1, 2]]}),
        json!({"layer": id, "tip": [10, 10], "bubbleRadius": 0}),
        json!({"layer": id, "tip": [10, 10], "mouthWidth": 1e9}),
        json!({"layer": id, "preset": 3, "tip": [10, 10]}),
        json!({"layer": 9999, "tip": [10, 10]}),
    ];
    for params in cases {
        assert!(s.execute("shape.balloon.tail", params.clone()).is_err(), "{params}");
        assert_eq!(doc(&s), &before, "no edit for {params}");
        assert_eq!(s.active().unwrap().history.past_len(), history);
    }
    let cases = [
        json!({"kind": "cloud"}),
        json!({"kind": "cloud", "rect": [0, 0, 0, 10]}),
        json!({"kind": "spiky", "rect": [0, 0, 10, 10], "spikes": "many"}),
        json!({"kind": "blob", "rect": [0, 0, 10, 10]}),
        json!({"kind": "cloud", "rect": [0, 0, 1e300, 10]}),
    ];
    for params in cases {
        assert!(s.execute("shape.balloon.create", params.clone()).is_err(), "{params}");
        assert_eq!(doc(&s), &before);
    }
    let cases = [
        json!({}),
        json!({"points": []}),
        json!({"points": [[0, 0]]}),
        json!({"points": [[0, 0], [1e300, 0]]}),
        json!({"points": [{"x": 0, "y": 0}, {"y": 10}]}),
        json!({"points": [[0, 0], [10, 10]], "width": 0}),
        json!({"points": [[0, 0], [10, 10]], "width": -3}),
        json!({"points": [[0, 0], [10, 10]], "tapered": "yes"}),
    ];
    for params in cases {
        assert!(s.execute("shape.balloon.stroke", params.clone()).is_err(), "{params}");
        assert_eq!(doc(&s), &before);
    }
    assert_eq!(s.active().unwrap().history.past_len(), history);
}

#[test]
fn presets_are_listed_for_the_ui() {
    let mut s = session(50, 50);
    let presets = s.execute("shape.balloon.presets", json!({})).unwrap();
    let ids: Vec<&str> = presets["presets"].as_array().unwrap().iter().filter_map(|p| p["id"].as_str()).collect();
    assert_eq!(ids, ["wedge", "arc", "double", "thought"]);
}

/// A parametric balloon payload, the way the gizmo sends it.
fn live_balloon(body: serde_json::Value) -> serde_json::Value {
    json!({
        "id": 1,
        "body": body,
        "tails": [],
        "nextTailId": 1,
        "vertexOverrides": [],
        "handDrawnNoise": 0.0,
        "strokeWidth": 3.0,
        "autoMergeOverlapping": true
    })
}

/// The balloon group of a layer, or a panic when the layer is not one.
fn balloon_group(s: &Session, id: u64) -> LiveBalloonGroup {
    match shape(s, id).live {
        Some(LiveShape::BalloonGroup { group }) => group,
        _ => panic!("the layer is not a balloon group"),
    }
}

#[test]
fn a_live_balloon_becomes_a_parametric_layer_and_undoes_in_one_step() {
    let mut s = session(300, 300);
    let created = s
        .execute("shape.balloon.create", json!({"balloon": live_balloon(json!({"bodyKind": "ellipse", "rect": [20.0, 20.0, 200.0, 140.0], "squircle": 0.2}))}))
        .unwrap();
    let id = created["layer"].as_u64().unwrap();
    let sh = shape(&s, id);
    assert!(matches!(sh.live, Some(LiveShape::BalloonGroup { .. })), "the layer keeps the parametric balloon");
    assert!(!sh.path.subpaths.is_empty(), "the balloon has a drawable outline");

    // Replacing the balloon is one undoable step.
    let history = s.active().unwrap().history.past_len();
    s.execute(
        "shape.balloon.update",
        json!({"layer": id, "balloon": live_balloon(json!({"bodyKind": "rect", "rect": [40.0, 40.0, 120.0, 90.0], "cornerRadius": 8.0}))}),
    )
    .unwrap();
    assert_eq!(s.active().unwrap().history.past_len(), history + 1, "one undoable step");
    assert_eq!(balloon_group(&s, id).items[0].body.rect(), [40.0, 40.0, 120.0, 90.0]);
}

#[test]
fn a_live_balloon_tail_is_added_removed_and_converted_to_linework() {
    let mut s = session(300, 300);
    let id = s
        .execute("shape.balloon.create", json!({"balloon": live_balloon(json!({"bodyKind": "ellipse", "rect": [40.0, 40.0, 160.0, 120.0], "squircle": 0.0}))}))
        .unwrap()["layer"]
        .as_u64()
        .unwrap();

    let added = s.execute("shape.balloon.tail.add", json!({"layer": id, "balloonId": 1, "tip": [120.0, 240.0], "preset": "arc"})).unwrap();
    let tail_id = added["tailId"].as_u64().expect("the tail has an id");
    assert_eq!(balloon_group(&s, id).items[0].tails.len(), 1);

    s.execute("shape.balloon.tail.remove", json!({"layer": id, "balloonId": 1, "tailId": tail_id})).unwrap();
    assert!(balloon_group(&s, id).items[0].tails.is_empty(), "the tail is gone");

    let converted = s.execute("shape.balloon.convertToLinework", json!({"layer": id})).unwrap();
    assert!(converted["convertedStrokes"].as_u64().unwrap() > 0, "the balloon became linework");
    let sh = shape(&s, id);
    assert!(sh.live.is_none(), "the balloon is clean linework now");
    assert!(!sh.path.subpaths.is_empty());
}

#[test]
fn hostile_live_balloon_params_are_errors_without_editing_the_document() {
    let mut s = session(300, 300);
    let id = ellipse_layer(&mut s);
    let before = doc(&s).clone();
    let history = s.active().unwrap().history.past_len();

    // A balloon that cannot be drawn is refused.
    let cases = [
        json!({"balloon": live_balloon(json!({"bodyKind": "ellipse", "rect": [0.0, 0.0, 0.0, 10.0], "squircle": 0.0}))}),
        json!({"balloon": live_balloon(json!({"bodyKind": "ellipse", "rect": [0.0, 0.0, 1e300, 10.0], "squircle": 0.0}))}),
        json!({"balloon": live_balloon(json!({"bodyKind": "nope", "rect": [0.0, 0.0, 10.0, 10.0]}))}),
        json!({"balloon": {"id": 1, "body": {"bodyKind": "ellipse", "rect": [0.0, 0.0, 10.0, 10.0]}, "strokeWidth": 0.0}}),
    ];
    for params in cases {
        assert!(s.execute("shape.balloon.create", params.clone()).is_err(), "{params}");
        assert_eq!(doc(&s), &before, "no edit for {params}");
    }

    // A plain shape layer is not a balloon group: the group commands refuse it.
    let cases = [
        json!({"layer": id, "balloon": live_balloon(json!({"bodyKind": "ellipse", "rect": [0.0, 0.0, 10.0, 10.0], "squircle": 0.0}))}),
        json!({"layer": id, "balloonId": 1, "tip": [10.0, 10.0]}),
        json!({"layer": id, "balloonId": 1, "tailId": 1}),
        json!({"layer": id}),
        json!({"layer": 9999, "balloonId": 1, "tip": [10.0, 10.0]}),
    ];
    for params in cases {
        assert!(s.execute("shape.balloon.update", params.clone()).is_err(), "{params}");
        assert!(s.execute("shape.balloon.tail.add", params.clone()).is_err(), "{params}");
        assert!(s.execute("shape.balloon.tail.remove", params.clone()).is_err(), "{params}");
        assert!(s.execute("shape.balloon.convertToLinework", params.clone()).is_err(), "{params}");
        assert_eq!(doc(&s), &before, "no edit for {params}");
    }

    // A tail with a broken tip or an unknown preset is refused too.
    let live_id = s
        .execute("shape.balloon.create", json!({"balloon": live_balloon(json!({"bodyKind": "ellipse", "rect": [40.0, 40.0, 120.0, 90.0], "squircle": 0.0}))}))
        .unwrap()["layer"]
        .as_u64()
        .unwrap();
    let with_balloon = doc(&s).clone();
    let cases = [
        json!({"layer": live_id, "balloonId": 1, "tip": [10.0]}),
        json!({"layer": live_id, "balloonId": 1, "tip": [1e300, 0.0]}),
        json!({"layer": live_id, "balloonId": 1, "tip": [10.0, 10.0], "preset": "nope"}),
        json!({"layer": live_id, "balloonId": 1, "tip": [10.0, 10.0], "mouthWidth": 0.0}),
        json!({"layer": live_id, "balloonId": 7, "tip": [10.0, 10.0]}),
        json!({"layer": live_id, "balloonId": 1}),
    ];
    for params in cases {
        assert!(s.execute("shape.balloon.tail.add", params.clone()).is_err(), "{params}");
        assert_eq!(doc(&s), &with_balloon, "no edit for {params}");
    }
    assert!(s.execute("shape.balloon.tail.remove", json!({"layer": live_id, "balloonId": 1})).is_err(), "a tail id is required");

    assert_eq!(s.active().unwrap().history.past_len(), history + 1, "only the balloon creation is a step");
}

#[test]
fn batch_from_script_and_svg_import_export_roundtrip() {
    let mut s = session(600, 800);
    let batch = s.execute("shape.balloon.batchFromScript", json!({"script": "Привет!\nОсторожно!!", "sizePt": 14.0, "vertical": false})).unwrap();
    assert_eq!(batch["createdBalloons"].as_u64(), Some(2));
    let layer_id = batch["layer"].as_u64().unwrap();
    let group = balloon_group(&s, layer_id);
    assert_eq!(group.items.len(), 2);
    assert!(matches!(group.items[1].body, LiveBalloonBody::Spiky { .. }));

    // Export standalone SVG with fitted dialogue.
    let exp = s.execute("shape.balloon.exportSvg", json!({"layer": layer_id, "balloonId": 1, "text": "Привет!", "sizePt": 14.0})).unwrap();
    let svg = exp["svg"].as_str().unwrap();
    assert!(svg.contains("<svg") && svg.contains("<path") && svg.contains("<text"));

    // Import valid custom contour and reject self-intersecting bowtie.
    let imp = s.execute("shape.balloon.importSvg", json!({"layer": layer_id, "balloonId": 1, "svg": "M 0 0 L 100 0 L 100 80 L 0 80 Z"})).unwrap();
    assert_eq!(imp["ok"].as_bool(), Some(true));
    assert_eq!(imp["importedKnots"].as_u64(), Some(4));

    assert!(
        s.execute("shape.balloon.importSvg", json!({"layer": layer_id, "balloonId": 1, "svg": "M 0 0 L 100 80 L 100 0 L 0 80 Z"})).is_err(),
        "self-intersecting SVG contour must be rejected"
    );
}

#[test]
fn batch_from_script_scales_geometry_on_a_large_canvas() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 12_303, "height": 17_499, "background": "transparent"})).unwrap();
    let info = s.execute("shape.balloon.batchFromScript", json!({"lines": ["Hi!"]})).unwrap();
    let id = info["layer"].as_u64().unwrap();
    let sh = shape(&s, id);
    let stroke_width = sh.stroke.as_ref().map(|st| st.width).unwrap_or(0.0);
    assert!(stroke_width > 2.5 * 10.0, "stroke width scales with the page: {stroke_width}");
    let Some(LiveShape::BalloonGroup { group }) = sh.live.as_ref() else {
        panic!("batch created a BalloonGroup");
    };
    let item = &group.items[0];
    let tail = item.tails.first().expect("batch balloons carry an Arc tail");
    assert!(tail.mouth_width > 24.0 * 10.0, "mouth width scales: {}", tail.mouth_width);
    assert!(tail.bubble_radius > 10.0 * 10.0, "bubble radius scales: {}", tail.bubble_radius);
    let rect = item.body.rect();
    assert!(tail.tip[1] - (rect[1] + rect[3]) > 36.0 * 10.0, "tail tip offset scales: {}", tail.tip[1] - (rect[1] + rect[3]));
}

#[test]
fn double_outline_chamfer_rect_and_brush_preset_validation() {
    let mut s = session(400, 400);
    let dbl = s.execute("shape.balloon.create", json!({"kind": "doubleOutline", "rect": [40, 40, 160, 100], "gap": 6.0})).unwrap();
    let dbl_id = dbl["layer"].as_u64().unwrap();
    assert!(!shape(&s, dbl_id).path.subpaths.is_empty());

    let mut ch_payload = live_balloon(json!({"bodyKind": "chamferRect", "rect": [40.0, 40.0, 180.0, 110.0], "chamfer": 16.0, "concave": 0.4}));
    ch_payload["lineStyle"] = json!("ukiyoBrush");
    ch_payload["ukiyoContrast"] = json!(0.8);
    ch_payload["brushPresetId"] = json!("fukidashi-fude");
    let ch = s.execute("shape.balloon.create", json!({"balloon": ch_payload})).unwrap();
    let ch_id = ch["layer"].as_u64().unwrap();
    assert_eq!(balloon_group(&s, ch_id).items[0].brush_preset_id.as_deref(), Some("fukidashi-fude"));

    let mut bad_payload = live_balloon(json!({"bodyKind": "ellipse", "rect": [20.0, 20.0, 100.0, 80.0]}));
    bad_payload["brushPresetId"] = json!("../escape");
    assert!(s.execute("shape.balloon.create", json!({"balloon": bad_payload})).is_err());
}

#[test]
fn dual_balloon_add_merge_split_and_stories_reflow_undo_cleanly() {
    let mut s = session(800, 600);
    // Create two separate balloons with two linked Story text layers.
    let b1 = live_balloon(json!({"bodyKind": "ellipse", "rect": [300.0, 80.0, 200.0, 120.0]}));
    let l_b1 = s.execute("shape.balloon.create", json!({"balloon": b1})).unwrap()["layer"].as_u64().unwrap();
    let b2 = live_balloon(json!({"bodyKind": "ellipse", "rect": [100.0, 180.0, 200.0, 140.0]}));
    let l_b2 = s.execute("shape.balloon.create", json!({"balloon": b2})).unwrap()["layer"].as_u64().unwrap();

    let t1 = s
        .execute(
            "type.create",
            json!({"box": [330, 100, 140, 80], "text": "One two three four five six seven eight nine ten", "size": 14, "storyThreaded": true}),
        )
        .unwrap()["layer"]
        .as_u64()
        .unwrap();
    let t2 = s.execute("type.create", json!({"box": [130, 200, 140, 100], "text": "", "size": 14, "storyThreaded": true})).unwrap()["layer"].as_u64().unwrap();
    s.execute("type.edit", json!({"layer": t1, "storyThreaded": true, "nextStoryLayer": t2})).unwrap();

    // Link each balloon to its text layer, then merge the two balloons into one group.
    let mut upd1 = live_balloon(json!({"bodyKind": "ellipse", "rect": [300.0, 80.0, 200.0, 120.0]}));
    upd1["id"] = json!(1);
    upd1["linkedTextLayer"] = json!(t1);
    s.execute("shape.balloon.update", json!({"layer": l_b1, "balloon": upd1})).unwrap();

    let mut upd2 = live_balloon(json!({"bodyKind": "ellipse", "rect": [100.0, 180.0, 200.0, 140.0]}));
    upd2["id"] = json!(1);
    upd2["linkedTextLayer"] = json!(t2);
    s.execute("shape.balloon.update", json!({"layer": l_b2, "balloon": upd2})).unwrap();

    let merge = s.execute("shape.balloon.merge", json!({"layers": [l_b1, l_b2]})).unwrap();
    assert_eq!(merge["balloonCount"].as_u64(), Some(2));
    assert!(doc(&s).layer(LayerId(l_b2)).is_none(), "absorbed balloon layer is removed");
    let merged = balloon_group(&s, l_b1);
    assert_eq!(merged.items[0].linked_text_layer, Some(LayerId(t1)));
    assert_eq!(merged.items[1].linked_text_layer, Some(LayerId(t2)));

    // Shrinking chamber 1 triggers live Stories box update + text overflow into t2 in one step.
    let hist_before = s.active().unwrap().history.past_len();
    let mut shrink1 = serde_json::to_value(&merged.items[0]).unwrap();
    shrink1["body"] = json!({"bodyKind": "ellipse", "rect": [300.0, 80.0, 110.0, 56.0], "squircle": 0.25});
    s.execute("shape.balloon.update", json!({"layer": l_b1, "balloon": shrink1, "coalesce": "drag-story"})).unwrap();
    assert_eq!(s.active().unwrap().history.past_len(), hist_before + 1);
    let text_b_after_shrink = match &doc(&s).layer(LayerId(t2)).unwrap().content {
        LayerContent::Text(tl) => tl.text.clone(),
        _ => panic!("expected text layer"),
    };
    assert!(!text_b_after_shrink.trim().is_empty(), "shrunk chamber 1 must push overflow into story chamber 2");

    // Growing chamber 1 pulls text back from t2.
    let mut grow1 = serde_json::to_value(&balloon_group(&s, l_b1).items[0]).unwrap();
    grow1["body"] = json!({"bodyKind": "ellipse", "rect": [260.0, 60.0, 420.0, 280.0], "squircle": 0.25});
    s.execute("shape.balloon.update", json!({"layer": l_b1, "balloon": grow1, "coalesce": "drag-story"})).unwrap();
    assert_eq!(s.active().unwrap().history.past_len(), hist_before + 1, "coalesced drag stays one undo step");
    let text_b_after_grow = match &doc(&s).layer(LayerId(t2)).unwrap().content {
        LayerContent::Text(tl) => tl.text.clone(),
        _ => panic!("expected text layer"),
    };
    assert!(text_b_after_grow.trim().is_empty(), "enlarged chamber 1 must pull text back from t2: {text_b_after_grow:?}");

    // Undo restores both text boxes & balloon shape cleanly.
    s.execute("edit.undo", json!({})).unwrap();
    let t2_undone = match &doc(&s).layer(LayerId(t2)).unwrap().content {
        LayerContent::Text(tl) => tl.text.clone(),
        _ => panic!("expected text layer"),
    };
    assert!(t2_undone.is_empty());

    // Add a 3rd chamber and split one back out.
    let added = s
        .execute("shape.balloon.add", json!({"layer": l_b1, "balloon": live_balloon(json!({"bodyKind": "ellipse", "rect": [50.0, 50.0, 90.0, 70.0]}))}))
        .unwrap();
    assert_eq!(added["balloonId"].as_u64(), Some(3));
    let split = s.execute("shape.balloon.split", json!({"layer": l_b1, "balloonId": 2})).unwrap();
    let new_layer = split["newLayer"].as_u64().unwrap();
    assert_eq!(balloon_group(&s, new_layer).items[0].linked_text_layer, Some(LayerId(t2)));
}

fn fill_rgba(fill: &Fill) -> Option<[f32; 4]> {
    match fill {
        Fill::Solid(c) => Some([c.c[0], c.c[1], c.c[2], c.alpha]),
        _ => None,
    }
}

/// A black stylized balloon with white ink, ready to be baked to SAI linework.
fn inverted_stipple_balloon(gloom: bool) -> serde_json::Value {
    let mut balloon = live_balloon(json!({"bodyKind": "ellipse", "rect": [40.0, 40.0, 200.0, 140.0], "squircle": 0.2}));
    balloon["lineStyle"] = json!("stippleDots");
    balloon["stippleCount"] = json!(120);
    balloon["stippleDensity"] = json!(100.0);
    balloon["stippleFillInterior"] = json!(gloom);
    balloon
}

fn backing_fill_layer(s: &Session, source_name: &str) -> Option<ShapeLayer> {
    let name = format!("{source_name} fill");
    let layer = doc(s).walk().into_iter().find(|(_, _, l)| l.name == name).map(|(_, _, l)| l)?;
    match &doc(s).layer(layer.id).unwrap().content {
        LayerContent::Shape(sh) => Some(sh.clone()),
        _ => None,
    }
}

#[test]
fn convert_to_linework_preserves_white_ink_on_a_black_balloon() {
    let mut s = session(400, 400);
    let created = s
        .execute("shape.balloon.create", json!({"balloon": inverted_stipple_balloon(false), "fill": "#000000", "stroke": {"width": 3.0, "color": "#ffffff"}}))
        .unwrap();
    let id = created["layer"].as_u64().unwrap();
    let baked = s.execute("shape.balloon.convertToLinework", json!({"layer": id})).unwrap();
    assert!(baked["convertedStrokes"].as_u64().unwrap() >= 120, "every dot is baked");

    let sh = shape(&s, id);
    assert!(sh.live.is_none(), "the balloon is clean linework now");
    assert!(sh.stroke.is_none());
    let ink = fill_rgba(sh.fill.as_ref().unwrap()).unwrap();
    assert!(ink[0] > 0.98 && ink[1] > 0.98 && ink[2] > 0.98 && ink[3] > 0.98, "white ink stays white: {ink:?}");
}

#[test]
fn convert_to_linework_preserves_black_fill_in_gloom_mode() {
    let mut s = session(400, 400);
    // In Gloom mode the shape's `fill` stays white (`#ffffff`) while the solid interior is
    // drawn in the ink pass (`stroke = #000000`). Baking must copy that black ink onto the
    // backing fill layer instead of keeping the stale white fill.
    let created = s
        .execute("shape.balloon.create", json!({"balloon": inverted_stipple_balloon(true), "fill": "#ffffff", "stroke": {"width": 3.0, "color": "#000000"}}))
        .unwrap();
    let id = created["layer"].as_u64().unwrap();
    let name = created["name"].as_str().unwrap();
    s.execute("shape.balloon.convertToLinework", json!({"layer": id})).unwrap();

    let backing = backing_fill_layer(&s, name).expect("a backing fill layer was created");
    assert!(!backing.path.subpaths.is_empty(), "the clean body fill survives the bake");
    let fill = fill_rgba(backing.fill.as_ref().unwrap()).unwrap();
    assert!(fill[0] < 0.02 && fill[1] < 0.02 && fill[2] < 0.02 && fill[3] > 0.98, "Gloom black fill stays black: {fill:?}");

    let ink = fill_rgba(shape(&s, id).fill.as_ref().unwrap()).unwrap();
    assert!(ink[0] < 0.02 && ink[1] < 0.02 && ink[2] < 0.02, "the Gloom ink dots stay black: {ink:?}");
}

#[test]
fn convert_to_linework_preserves_black_paper_fill() {
    let mut s = session(400, 400);
    let created = s
        .execute("shape.balloon.create", json!({"balloon": inverted_stipple_balloon(false), "fill": "#000000", "stroke": {"width": 3.0, "color": "#ffffff"}}))
        .unwrap();
    let id = created["layer"].as_u64().unwrap();
    let name = created["name"].as_str().unwrap();
    s.execute("shape.balloon.convertToLinework", json!({"layer": id})).unwrap();

    let backing = backing_fill_layer(&s, name).expect("a backing fill layer was created");
    assert!(!backing.path.subpaths.is_empty());
    let fill = fill_rgba(backing.fill.as_ref().unwrap()).unwrap();
    assert!(fill[0] < 0.02 && fill[1] < 0.02 && fill[2] < 0.02 && fill[3] > 0.98, "black Paper fill stays black: {fill:?}");
}

// --- Auto-follow / text-fit-mode regression coverage -------------------------------------------

fn text_layer(s: &Session, id: u64) -> photocraft_doc::TextLayer {
    match &doc(s).layer(LayerId(id)).unwrap().content {
        LayerContent::Text(t) => t.clone(),
        other => panic!("not a text layer: {}", other.kind_name()),
    }
}

fn box_origin(t: &photocraft_doc::TextLayer) -> [f64; 2] {
    match t.shape {
        photocraft_doc::text::TextShape::Box { .. } => [t.transform.m[4], t.transform.m[5]],
        ref other => panic!("expected a box text shape, got {other:?}"),
    }
}

fn box_size(t: &photocraft_doc::TextLayer) -> [f32; 2] {
    match t.shape {
        photocraft_doc::text::TextShape::Box { width, height, .. } => [width, height],
        ref other => panic!("expected a box text shape, got {other:?}"),
    }
}

fn linked_balloon(s: &mut Session, rect: [f64; 4], tid: u64) -> u64 {
    let b = LiveBalloon { id: 1, body: LiveBalloonBody::Ellipse { rect, squircle: 0.0 }, tails: Vec::new(), ..LiveBalloon::default() };
    let layer = s.execute("shape.balloon.create", json!({ "balloon": b })).unwrap()["layer"].as_u64().unwrap();
    let mut b = balloon_group(s, layer).items[0].clone();
    b.linked_text_layer = Some(LayerId(tid));
    s.execute("shape.balloon.update", json!({ "layer": layer, "balloon": b })).unwrap();
    layer
}

fn push_balloon(s: &mut Session, layer: u64, f: impl FnOnce(&mut LiveBalloon)) {
    let mut b = balloon_group(s, layer).items[0].clone();
    f(&mut b);
    s.execute("shape.balloon.update", json!({ "layer": layer, "balloon": b })).unwrap();
}

#[test]
fn switching_text_fit_mode_restructures_the_same_linked_text_layer() {
    let mut s = session(800, 600);
    let text = s.execute("type.create", json!({ "box": [120, 120, 200, 100], "text": "Hello", "size": 14 })).unwrap()["layer"]
        .as_u64()
        .unwrap();
    let shape_id = linked_balloon(&mut s, [100.0, 100.0, 400.0, 240.0], text);
    let text_count = doc(&s).layers.iter().filter(|l| matches!(l.content, LayerContent::Text(_))).count();

    // Safe -> a box on the safe rect.
    assert!(matches!(text_layer(&s, text).shape, photocraft_doc::text::TextShape::Box { .. }));
    let safe_w = box_size(&text_layer(&s, text))[0];
    let safe_origin = box_origin(&text_layer(&s, text));

    // Outer -> the same layer, a wider box on the tail-free body bounds.
    push_balloon(&mut s, shape_id, |b| b.text_fit_mode = BalloonTextFitMode::Outer);
    assert!(matches!(text_layer(&s, text).shape, photocraft_doc::text::TextShape::Box { .. }), "Outer stays a box");
    assert!(box_size(&text_layer(&s, text))[0] > safe_w, "Outer uses the full body width");

    // Contour -> the same layer becomes area type, not a new layer.
    push_balloon(&mut s, shape_id, |b| b.text_fit_mode = BalloonTextFitMode::Contour);
    let area = text_layer(&s, text);
    let photocraft_doc::text::TextShape::Area { path } = &area.shape else { panic!("Contour must be area type: {:?}", area.shape) };
    assert_eq!(path.subpaths.len(), 1);
    assert!(path.subpaths[0].closed);
    let body = balloon_group(&s, shape_id).items[0].clone();
    let doc_bounds = body.text_area_path().control_bounds().unwrap();
    // Area knots are stored in text space; mapping them back must land on the balloon contour.
    let first = path.subpaths[0].knots[0].anchor;
    let doc_first = photocraft_geom::Point::new(first.x * area.transform.m[0] + area.transform.m[4], first.y * area.transform.m[3] + area.transform.m[5]);
    assert!(doc_first.x >= doc_bounds.0 - 1.0 && doc_first.x <= doc_bounds.2 + 1.0, "area path lands at the balloon: {doc_first:?} vs {doc_bounds:?}");

    // Contour -> Safe flattens back to a box on the original safe rect, same layer id throughout.
    push_balloon(&mut s, shape_id, |b| b.text_fit_mode = BalloonTextFitMode::Safe);
    assert!(matches!(text_layer(&s, text).shape, photocraft_doc::text::TextShape::Box { .. }));
    assert_eq!(box_size(&text_layer(&s, text))[0], safe_w);
    assert_eq!(box_origin(&text_layer(&s, text)), safe_origin);
    assert_eq!(doc(&s).layers.iter().filter(|l| matches!(l.content, LayerContent::Text(_))).count(), text_count);
    assert!(doc(&s).layer(LayerId(text)).is_some());
}

#[test]
fn dragging_vertex_overrides_refits_linked_text_when_auto_follow_is_on() {
    let mut s = session(800, 600);
    let text = s.execute("type.create", json!({ "box": [120, 120, 200, 100], "text": "Hi", "size": 14 })).unwrap()["layer"]
        .as_u64()
        .unwrap();
    let shape_id = linked_balloon(&mut s, [100.0, 100.0, 400.0, 240.0], text);
    let before = box_origin(&text_layer(&s, text));

    push_balloon(&mut s, shape_id, |b| {
        b.vertex_overrides.push(photocraft_doc::vector::VertexOverride { index: 0, offset: [0.0, -40.0] });
    });
    let after = box_origin(&text_layer(&s, text));
    assert!(after[1] < before[1] - 4.0, "a dragged bulge moves the linked text: {before:?} -> {after:?}");

    // With auto-follow off, the same geometry change leaves the box alone.
    push_balloon(&mut s, shape_id, |b| {
        b.auto_follow_text = false;
        b.vertex_overrides.clear();
    });
    let frozen = box_origin(&text_layer(&s, text));
    push_balloon(&mut s, shape_id, |b| {
        b.vertex_overrides.push(photocraft_doc::vector::VertexOverride { index: 2, offset: [60.0, 0.0] });
    });
    assert_eq!(box_origin(&text_layer(&s, text)), frozen);
}

#[test]
fn move_and_transform_follow_the_balloon_without_double_moving_or_corrupting_overrides() {
    let mut s = session(800, 600);
    let text = s.execute("type.create", json!({ "box": [120, 120, 200, 100], "text": "Hi", "size": 14 })).unwrap()["layer"]
        .as_u64()
        .unwrap();
    let shape_id = linked_balloon(&mut s, [100.0, 100.0, 400.0, 240.0], text);
    let overrides = vec![photocraft_doc::vector::VertexOverride { index: 0, offset: [0.0, -20.0] }];
    push_balloon(&mut s, shape_id, |b| b.vertex_overrides = overrides.clone());

    // Move Tool: the lettering travels with its chamber and the override delta is untouched.
    let before = box_origin(&text_layer(&s, text));
    s.execute("layer.translate", json!({ "layer": shape_id, "dx": 50, "dy": 30 })).unwrap();
    let moved = box_origin(&text_layer(&s, text));
    assert!((moved[0] - before[0] - 50.0).abs() < 1e-6 && (moved[1] - before[1] - 30.0).abs() < 1e-6, "move follows: {before:?} -> {moved:?}");
    assert_eq!(balloon_group(&s, shape_id).items[0].vertex_overrides, overrides, "offsets are deltas, not points");

    // Free Transform: scaling the chamber scales the linked text box in the same step.
    let size_before = box_size(&text_layer(&s, text));
    s.execute("edit.transform", json!({ "layer": shape_id, "matrix": [2.0, 0.0, 0.0, 2.0, 0.0, 0.0] })).unwrap();
    let scaled = text_layer(&s, text);
    assert!((box_size(&scaled)[0] - size_before[0] * 2.0).abs() < 1.0, "text box scales with the balloon");
    assert_eq!(balloon_group(&s, shape_id).items[0].vertex_overrides.len(), 1);

    // Selecting both layers and translating must not refit the text. Nudge it off the safe rect
    // first so a stray sync would snap it back and make the delta wrong.
    s.execute("type.edit", json!({ "layer": text, "move": [13.0, 0.0] })).unwrap();
    let from = box_origin(&text_layer(&s, text));
    s.execute("layer.select", json!({ "layer": text, "mode": "add" })).unwrap();
    s.execute("layer.translate", json!({ "dx": 10, "dy": 0 })).unwrap();
    let final_origin = box_origin(&text_layer(&s, text));
    assert!((final_origin[0] - from[0] - 10.0).abs() < 1e-6, "a jointly selected text layer moves once: {from:?} -> {final_origin:?}");
}

/// The shared limits must agree with the UI sliders: values the slider can produce pass
/// validation, values just outside them are rejected (the old caps were 600/128 and bokeh was
/// not validated at all).
#[test]
fn validate_balloon_uses_the_shared_particle_limits() {
    let base = || LiveBalloon {
        id: 1,
        body: LiveBalloonBody::Ellipse { rect: [10.0, 10.0, 200.0, 140.0], squircle: 0.0 },
        ..LiveBalloon::default()
    };

    for (count, ok) in [(610, true), (2000, true), (2001, false), (23, false)] {
        let mut b = base();
        b.stipple_count = count;
        assert_eq!(validate_balloon(&b).is_ok(), ok, "stipple_count {count}");
    }
    for (count, ok) in [(400, true), (401, false)] {
        let mut b = base();
        b.sparkle_count = count;
        assert_eq!(validate_balloon(&b).is_ok(), ok, "sparkle_count {count}");
    }
    for (count, ok) in [(400, true), (401, false)] {
        let mut b = base();
        b.bokeh_count = count;
        assert_eq!(validate_balloon(&b).is_ok(), ok, "bokeh_count {count}");
    }
}
