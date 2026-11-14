pub use crate::input::ring_buffer::{
    InputRun, LayoutTag, RunKind, RunOrigin, mark_last_token_autoconverted_if_generation,
    push_runs_if_generation, take_last_layout_run_with_suffix_if_generation,
    take_last_sequence_with_suffix_if_generation,
};
