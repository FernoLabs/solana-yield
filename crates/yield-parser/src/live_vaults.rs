//! Compatibility exports for the original multi-protocol account API.
//! New code can import each reader from [`crate::protocols`].

pub use crate::protocols::{
    elemental::observe_elemental, hylo::observe_hylo, meteora::observe_meteora,
};
