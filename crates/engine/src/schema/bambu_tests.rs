use serde_json::json;

use super::*;
use crate::schema::tests::{assert_keys, assert_strict_keeps_todays_errors};
use crate::schema::{input_schema_for, output_schema_for};

#[test]
fn schemas_and_the_key_check_agree() {
    assert_keys(input_schema_for::<PresetsParams>(), &["printer", "process"]);
    assert_keys(input_schema_for::<PrintersParams>(), &["discover", "wait"]);
    assert_keys(
        input_schema_for::<SliceParams>(),
        &["arrange", "bed_type", "bodies", "filaments", "orient", "overrides", "placements", "printer", "process", "slots"],
    );
    assert_keys(input_schema_for::<OpenInStudioParams>(), &["bodies", "file", "slots"]);
    assert_keys(input_schema_for::<StatusParams>(), &["ip", "printer", "serial"]);
    assert_keys(input_schema_for::<ControlParams>(), &["action", "ip", "serial"]);
    assert_keys(
        input_schema_for::<PrintParams>(),
        &["bed_leveling", "file", "flow_cali", "ip", "mapping", "name", "nozzle_offset_cali", "serial", "timelapse"],
    );
}

#[test]
fn strict_keeps_todays_errors() {
    // refused before anything reaches Bambu Studio or the network
    assert_strict_keeps_todays_errors(&[("bambu.print", json!({"serial": "X", "file": "/nowhere/plate.stl"}))]);
}

#[test]
fn replies_have_schemas() {
    for s in [
        output_schema_for::<PresetsResult>(),
        output_schema_for::<PrintersResult>(),
        output_schema_for::<SliceResult>(),
        output_schema_for::<OpenInStudioResult>(),
        output_schema_for::<StatusResult>(),
        output_schema_for::<PrintResult>(),
        output_schema_for::<ControlResult>(),
    ] {
        assert!(s.as_value().get("properties").is_some());
    }
}
