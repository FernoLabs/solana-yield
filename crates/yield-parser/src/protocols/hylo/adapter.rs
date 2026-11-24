//! Common account-adapter entry point.

use crate::{
    adapter::{AccountRequirement, ObservationContext, ProtocolAdapter},
    live::Measure,
    ParseError,
};

#[derive(Clone, Copy, Debug)]
pub struct Adapter<'a> {
    pub config: &'a str,
    pub pool: &'a str,
    pub share_mint: &'a str,
}

impl ProtocolAdapter for Adapter<'_> {
    fn protocol(&self) -> &'static str {
        "hylo"
    }
    fn required_accounts(&self) -> Vec<AccountRequirement<'_>> {
        vec![
            AccountRequirement {
                role: "config",
                address: self.config,
            },
            AccountRequirement {
                role: "pool",
                address: self.pool,
            },
            AccountRequirement {
                role: "share_mint",
                address: self.share_mint,
            },
        ]
    }
    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<Measure>, ParseError> {
        super::observe(
            &context.account(self.config)?,
            &context.account(self.pool)?,
            &context.account(self.share_mint)?,
        )
    }
}
