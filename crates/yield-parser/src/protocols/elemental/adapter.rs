//! Common account-adapter entry point.

use crate::{
    adapter::{AccountRequirement, ObservationContext, ProtocolAdapter},
    live::Measure,
    ParseError,
};

#[derive(Clone, Copy, Debug)]
pub struct Adapter<'a> {
    pub pool: &'a str,
    pub asset_mint: &'a str,
}

impl ProtocolAdapter for Adapter<'_> {
    fn protocol(&self) -> &'static str {
        "elemental"
    }
    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![AccountRequirement {
            role: "pool",
            address: self.pool,
        }]
    }
    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        super::observe(&context.account(self.pool)?, self.asset_mint)
    }
}
