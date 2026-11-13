//! Old build compatibility (Task 9): the core of commit 6364009, the last
//! build without voice, next to this one.

#[test]
fn old_core_builds_next_to_the_new_one() {
    let old = lfd3k_core_old::Config::new("REV").expect("old config");
    let new = lfd3k_core::Config::new("REV").expect("new config");
    let _ = (lfd3k_core_old::Modem::new(old), lfd3k_core::Modem::new(new));
}
