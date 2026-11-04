// Builtin default policy: loop-script search parameters (LATS family).
// The search itself is script (a mod's loop entry); this policy owns the
// knobs a loop script reads through the `loop_policy_int(field, default)`
// capability native — last-wins over this default, compiled fallback
// below it (the native's `default` argument is the script's own copy).
//
//   width  — candidates expanded per frontier node
//   depth  — search rounds before the loop answers
//   beam   — frontier size kept between rounds (best-first pruning)

register_policy("lats-policy", #{
    width: 3,
    depth: 2,
    beam: 2,
});
