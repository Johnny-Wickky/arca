use super::*;

const T0: i64 = 1_772_323_200_000;
const HOUR_MS: i32 = 3_600_000;

fn text(v: Vec<u8>) -> String {
    String::from_utf8(v).unwrap()
}

#[test]
fn plan_and_refine_pass_through() {
    let t = plan_next(T0, 3 * HOUR_MS, 4);
    assert_eq!(t, reminder::plan_next(T0, i64::from(3 * HOUR_MS), 4));
    assert_eq!(refine(t, 3 * HOUR_MS, 3 * HOUR_MS), t);
    assert_eq!(refine(t, 3 * HOUR_MS, 4 * HOUR_MS), t - i64::from(HOUR_MS));
    // A negative random number is just bits.
    assert_eq!(plan_next(T0, 0, -1), reminder::plan_next(T0, 0, u32::MAX));
}

#[test]
fn fire_decisions_are_small_json() {
    let noon = T0 + 13 * 3_600_000;
    let j = text(on_fire_json(noon, 0, 0, 1, 1, 0));
    assert!(j.starts_with("{\"action\":\"show\",\"at\":"), "{j}");
    assert!(
        text(on_fire_json(noon, 0, 0, 0, 1, 0)).starts_with("{\"action\":\"disabled\",\"at\":0}")
    );
    assert!(text(on_fire_json(noon, 0, 0, 1, 0, 0)).contains("noPermission"));
    assert!(text(on_fire_json(T0 + 3_600_000, 0, 0, 1, 1, 0)).contains("quietHours"));
    assert!(text(on_fire_json(noon, 0, noon - 1000, 1, 1, 0)).contains("tooSoon"));
}

#[test]
fn start_decisions_are_small_json() {
    assert_eq!(text(on_start_json(0, T0, 0, 0, 0)), "{\"action\":\"off\",\"at\":0}");
    assert!(text(on_start_json(0, T0, 0, 1, 0)).contains("\"plan\""));
    assert_eq!(
        text(on_start_json(T0 + 1000, T0, 0, 1, 0)),
        format!("{{\"action\":\"keep\",\"at\":{}}}", T0 + 1000)
    );
}

#[test]
fn picking_gives_an_index_in_range() {
    assert_eq!(item_count(), 30);
    assert_eq!(pick(0), 0);
    assert_eq!(pick(31), 1);
    // A negative position is just a position.
    assert!((0..30).contains(&pick(-1)));
}
