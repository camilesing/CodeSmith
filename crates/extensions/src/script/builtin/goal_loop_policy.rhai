// Builtin default policy: goal-loop budget knobs (dsh goal state machine).
// The loop shape itself is script (the `goal-loop` mod's loop entry); this
// policy owns the numbers a goal loop reads through the
// `loop_policy_int("goal-loop-policy", field, default)` capability native
// — last-wins over this default, compiled fallback below it (the native's
// `default` argument is the script's own copy).
//
//   max_steps        — loop rounds before the goal pauses for a human
//   fail_streak_limit — consecutive failing tool batches before the goal
//                       blocks for a course change

register_policy("goal-loop-policy", #{
    max_steps: 24,
    fail_streak_limit: 3,
});
