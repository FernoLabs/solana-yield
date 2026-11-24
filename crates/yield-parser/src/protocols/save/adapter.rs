//! Common account-adapter entry point.

use crate::{
    adapter::{AccountRequirement, ObservationContext, ProtocolAdapter},
    live::Measure,
    ParseError,
};

#[derive(Clone, Copy, Debug)]
pub struct Adapter<'a> {
    pub reserve: &'a str,
    pub lending_market: &'a str,
    pub asset_mint: &'a str,
}

impl ProtocolAdapter for Adapter<'_> {
    fn protocol(&self) -> &'static str {
        "save"
    }
    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![AccountRequirement {
            role: "reserve",
            address: self.reserve,
        }]
    }
    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        super::observe(
            &context.account(self.reserve)?,
            self.lending_market,
            self.asset_mint,
        )
    }
}
