//! Common account-adapter entry point.

use crate::{
    adapter::{AccountRequirement, ObservationContext, ProtocolAdapter},
    live::Measure,
    ParseError,
};

#[derive(Clone, Copy, Debug)]
pub struct Adapter<'a> {
    pub vault: &'a str,
    pub share_mint: &'a str,
    pub asset_mint: &'a str,
}

impl ProtocolAdapter for Adapter<'_> {
    fn protocol(&self) -> &'static str {
        "meteora"
    }
    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![
            AccountRequirement {
                role: "vault",
                address: self.vault,
            },
            AccountRequirement {
                role: "share_mint",
                address: self.share_mint,
            },
        ]
    }
    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        super::observe(
            &context.account(self.vault)?,
            &context.account(self.share_mint)?,
            self.asset_mint,
            context.unix_seconds(),
        )
    }
}
