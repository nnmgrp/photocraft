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
