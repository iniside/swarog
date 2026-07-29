use sqlx::PgConnection;
use walletapi::{Movement, MAX_MOVEMENT_AMOUNT};

use crate::{Outcome, Service};

/// The starter grant is OFF unless configured: an empty currency and a zero amount are the
/// compiled defaults, so a wallet nobody configured grants nothing. Enabling it is a
/// `config` write (`wallet/starter_currency`, `wallet/starter_amount`), never an env var or
/// a code change.
pub(crate) const STARTER_CURRENCY: &str = "";
pub(crate) const STARTER_AMOUNT: i64 = 0;

pub(crate) const REASON: &str = "starter-grant";

impl Service {
    /// Reads the starter knobs straight off the injected `config` reader — no wallet-owned
    /// second cache: the reader is itself a replica-local cache kept fresh by the app-owned
    /// broadcast invalidation plane, so another one would only add a staleness window.
    fn starter_spec(&self) -> (String, i64) {
        let cfg = self
            .config
            .get()
            .expect("wallet.init must resolve config before use");
        (
            cfg.get_string("wallet", "starter_currency", STARTER_CURRENCY),
            cfg.get_int("wallet", "starter_amount", STARTER_AMOUNT),
        )
    }

    /// Credits a brand-new player the configured starter amount. `conn` is the plane's
    /// HANDED delivery transaction (never the pool), so the balance, the ledger row, the
    /// `wallet.changed` append and the subscription checkpoint commit as one unit.
    ///
    /// **Every data-quality verdict returns `Ok(())`.** An `Err` here backs the subscription
    /// off and, after 20 consecutive failures, PAUSES `wallet.player-registered.v1` for every
    /// subsequent player — a fat-fingered config knob or an unseeded catalog must never cost
    /// that, because the fault is a property of the config, not of the event.
    ///
    /// The two pre-checks are therefore ORDERING-CRITICAL, not defensive: [`Service::apply_on`]
    /// validates the movement itself, so anything `validate_movement` would reject arrives as
    /// an `Err`; and letting the currency FK fire would abort the delivery transaction, after
    /// which even an `Ok` fails the plane's checkpoint `UPDATE` with 25P02. With the clamp
    /// below, a catalog whose own `currencies_code_len_check` caps a code at 32 octets, and
    /// fixed `reason` / key shapes, every one of those branches is unreachable BY
    /// CONSTRUCTION — which is why no `validate_movement` call is repeated here.
    pub(crate) async fn grant_starter(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), bus::Error> {
        let (currency, amount) = self.starter_spec();
        if currency.is_empty() || amount <= 0 {
            return Ok(());
        }
        if amount > MAX_MOVEMENT_AMOUNT {
            tracing::warn!(
                amount,
                max = MAX_MOVEMENT_AMOUNT,
                "wallet: configured starter_amount out of range — granting nothing"
            );
            return Ok(());
        }
        if !self
            .store
            .currency_exists_tx(&mut *conn, &currency)
            .await
            .map_err(bus::Error::transport)?
        {
            tracing::warn!(
                %currency,
                "wallet: configured starter_currency is not in the catalog — granting nothing"
            );
            return Ok(());
        }

        let movement = Movement {
            idempotency_key: format!("starter:{player_id}"),
            player_id: player_id.to_string(),
            currency,
            amount,
            reason: REASON.to_string(),
        };
        match self
            .apply_on(conn, &movement, 1)
            .await
            .map_err(bus::Error::transport)?
        {
            Outcome::Applied(_) | Outcome::Duplicate(_) => Ok(()),
            Outcome::Conflict => {
                tracing::warn!(
                    player_id,
                    key = %movement.idempotency_key,
                    "wallet: starter-grant key already records a different movement — granting nothing"
                );
                Ok(())
            }
        }
    }
}
