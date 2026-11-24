//! Common account-adapter entry point.

use crate::{
    adapter::{AccountRequirement, ObservationContext, ProtocolAdapter},
    live::Measure,
    ParseError,
};

#[derive(Clone, Copy, Debug)]
pub struct Adapter<'a> {
    pub bank: &'a str,
    pub group: &'a str,
    pub asset_mint: &'a str,
}

impl ProtocolAdapter for Adapter<'_> {
    fn protocol(&self) -> &'static str {
        "project0"
    }
    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![AccountRequirement {
            role: "bank",
            address: self.bank,
        }]
    }
    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        super::observe(&context.account(self.bank)?, self.group, self.asset_mint)
    }
}
