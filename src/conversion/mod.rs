pub mod input;
pub(crate) mod input_delivery;

pub use crate::domain::text::{
    convert::{convert_selection, smart_convert_selection},
    last_word::{
        convert_last_sequence, convert_last_word, convert_last_word_if_any, smart_convert_last_sequence,
        smart_convert_last_word,
    },
};
