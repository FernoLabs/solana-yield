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
    pub share_mint: &'a str,
}

impl ProtocolAdapter for Adapter<'_> {
    fn protocol(&self) -> &'static str {
        "kamino"
    }
    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![AccountRequirement {
            role: "reserve",
            address: self.reserve,
        }]
    }
    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        let value = super::share_value(
            &context.account(self.reserve)?,
            self.lending_market,
            self.asset_mint,
            self.share_mint,
        )?;
        Ok(vec![Measure {
            kind: "stored_assets_per_share",
            numerator: value.numerator(),
            denominator: value.denominator(),
            updated_slot: value.last_update_slot(),
            updated_unix_seconds: value.last_update_unix_seconds(),
        }])
    }
}
