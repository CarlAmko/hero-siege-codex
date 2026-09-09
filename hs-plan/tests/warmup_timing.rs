use std::time::Instant;

#[test]
fn time_calc_warmup() {
    let start = Instant::now();
    let ok = hs_plan::calc::commands::run_warmup(|_, _| {});
    let elapsed = start.elapsed();
    println!("CALC_WARMUP ok={ok} elapsed={elapsed:?}");
    assert!(ok);
}
