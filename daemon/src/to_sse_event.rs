use crate::model::cfd::{OrderId, Role};
use crate::model::{Leverage, Position, TradingPair, Usd};
use crate::{bitmex_price_feed, model};
use bdk::bitcoin::{Amount, SignedAmount};
use rocket::request::FromParam;
use rocket::response::stream::Event;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct Cfd {
    pub order_id: OrderId,
    pub initial_price: Usd,

    pub leverage: Leverage,
    pub trading_pair: TradingPair,
    pub position: Position,
    pub liquidation_price: Usd,

    pub quantity_usd: Usd,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin: Amount,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub profit_btc: SignedAmount,
    pub profit_in_percent: String,

    pub state: CfdState,
    pub actions: Vec<CfdAction>,
    pub state_transition_timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CfdAction {
    Accept,
    Reject,
    Commit,
    Settle,
}

impl<'v> FromParam<'v> for CfdAction {
    type Error = serde_plain::Error;

    fn from_param(param: &'v str) -> Result<Self, Self::Error> {
        let action = serde_plain::from_str(param)?;
        Ok(action)
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum CfdState {
    OutgoingOrderRequest,
    IncomingOrderRequest,
    Accepted,
    Rejected,
    ContractSetup,
    PendingOpen,
    Open,
    PendingCommit,
    OpenCommitted,
    MustRefund,
    Refunded,
    SetupFailed,
}

#[derive(Debug, Clone, Serialize)]
pub struct CfdOrder {
    pub id: OrderId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Usd,

    pub min_quantity: Usd,
    pub max_quantity: Usd,

    pub leverage: Leverage,
    pub liquidation_price: Usd,

    pub creation_timestamp: u64,
    pub term_in_secs: u64,
}

pub trait ToSseEvent {
    fn to_sse_event(&self) -> Event;
}

/// Intermediate struct to able to piggy-back current price along with cfds
pub struct CfdsWithCurrentPrice {
    pub cfds: Vec<model::cfd::Cfd>,
    pub current_price: Usd,
}

impl ToSseEvent for CfdsWithCurrentPrice {
    // TODO: This conversion can fail, we might want to change the API
    fn to_sse_event(&self) -> Event {
        let current_price = self.current_price;

        let cfds = self
            .cfds
            .iter()
            .map(|cfd| {
                let (profit_btc, profit_in_percent) =
                    cfd.profit(current_price).unwrap_or_else(|error| {
                        tracing::warn!(
                            "Calculating profit/loss failed. Falling back to 0. {:#}",
                            error
                        );
                        (SignedAmount::ZERO, Decimal::ZERO.into())
                    });

                Cfd {
                    order_id: cfd.order.id,
                    initial_price: cfd.order.price,
                    leverage: cfd.order.leverage,
                    trading_pair: cfd.order.trading_pair.clone(),
                    position: cfd.position(),
                    liquidation_price: cfd.order.liquidation_price,
                    quantity_usd: cfd.quantity_usd,
                    profit_btc,
                    profit_in_percent: profit_in_percent.to_string(),
                    state: cfd.state.clone().into(),
                    actions: actions_for_state(cfd.state.clone(), cfd.role()),
                    state_transition_timestamp: cfd
                        .state
                        .get_transition_timestamp()
                        .duration_since(UNIX_EPOCH)
                        .expect("timestamp to be convertable to duration since epoch")
                        .as_secs(),

                    // TODO: Depending on the state the margin might be set (i.e. in Open we save it
                    // in the DB internally) and does not have to be calculated
                    margin: cfd.margin().unwrap(),
                }
            })
            .collect::<Vec<Cfd>>();

        Event::json(&cfds).event("cfds")
    }
}

impl ToSseEvent for Option<model::cfd::Order> {
    fn to_sse_event(&self) -> Event {
        let order = self.clone().map(|order| CfdOrder {
            id: order.id,
            trading_pair: order.trading_pair,
            position: order.position,
            price: order.price,
            min_quantity: order.min_quantity,
            max_quantity: order.max_quantity,
            leverage: order.leverage,
            liquidation_price: order.liquidation_price,
            creation_timestamp: order
                .creation_timestamp
                .duration_since(UNIX_EPOCH)
                .expect("timestamp to be convertible to duration since epoch")
                .as_secs(),
            term_in_secs: order.term.as_secs(),
        });

        Event::json(&order).event("order")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletInfo {
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    balance: Amount,
    address: String,
    last_updated_at: u64,
}

impl ToSseEvent for model::WalletInfo {
    fn to_sse_event(&self) -> Event {
        let wallet_info = WalletInfo {
            balance: self.balance,
            address: self.address.to_string(),
            last_updated_at: into_unix_secs(self.last_updated_at),
        };

        Event::json(&wallet_info).event("wallet")
    }
}

impl From<model::cfd::CfdState> for CfdState {
    fn from(cfd_state: model::cfd::CfdState) -> Self {
        match cfd_state {
            model::cfd::CfdState::OutgoingOrderRequest { .. } => CfdState::OutgoingOrderRequest,
            model::cfd::CfdState::IncomingOrderRequest { .. } => CfdState::IncomingOrderRequest,
            model::cfd::CfdState::Accepted { .. } => CfdState::Accepted,
            model::cfd::CfdState::Rejected { .. } => CfdState::Rejected,
            model::cfd::CfdState::ContractSetup { .. } => CfdState::ContractSetup,
            model::cfd::CfdState::PendingOpen { .. } => CfdState::PendingOpen,
            model::cfd::CfdState::Open { .. } => CfdState::Open,
            model::cfd::CfdState::OpenCommitted { .. } => CfdState::OpenCommitted,
            model::cfd::CfdState::MustRefund { .. } => CfdState::MustRefund,
            model::cfd::CfdState::Refunded { .. } => CfdState::Refunded,
            model::cfd::CfdState::SetupFailed { .. } => CfdState::SetupFailed,
            model::cfd::CfdState::PendingCommit { .. } => CfdState::PendingCommit,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    bid: Usd,
    ask: Usd,
    last_updated_at: u64,
}

impl ToSseEvent for bitmex_price_feed::Quote {
    fn to_sse_event(&self) -> Event {
        let quote = Quote {
            bid: self.bid,
            ask: self.ask,
            last_updated_at: into_unix_secs(self.timestamp),
        };
        Event::json(&quote).event("quote")
    }
}

/// Convert to the format expected by the frontend
fn into_unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .expect("timestamp to be convertible to duration since epoch")
        .as_secs()
}

fn actions_for_state(state: model::cfd::CfdState, role: Role) -> Vec<CfdAction> {
    match (state, role) {
        (model::cfd::CfdState::IncomingOrderRequest { .. }, Role::Maker) => {
            vec![CfdAction::Accept, CfdAction::Reject]
        }
        (model::cfd::CfdState::Open { .. }, Role::Taker) => {
            vec![CfdAction::Commit, CfdAction::Settle]
        }
        (model::cfd::CfdState::Open { .. }, Role::Maker) => vec![CfdAction::Commit],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_snapshot_test() {
        // Make sure to update the UI after changing this test!

        let json = serde_json::to_string(&CfdState::OutgoingOrderRequest).unwrap();
        assert_eq!(json, "\"OutgoingOrderRequest\"");
        let json = serde_json::to_string(&CfdState::IncomingOrderRequest).unwrap();
        assert_eq!(json, "\"IncomingOrderRequest\"");
        let json = serde_json::to_string(&CfdState::Accepted).unwrap();
        assert_eq!(json, "\"Accepted\"");
        let json = serde_json::to_string(&CfdState::Rejected).unwrap();
        assert_eq!(json, "\"Rejected\"");
        let json = serde_json::to_string(&CfdState::ContractSetup).unwrap();
        assert_eq!(json, "\"ContractSetup\"");
        let json = serde_json::to_string(&CfdState::PendingOpen).unwrap();
        assert_eq!(json, "\"PendingOpen\"");
        let json = serde_json::to_string(&CfdState::Open).unwrap();
        assert_eq!(json, "\"Open\"");
        let json = serde_json::to_string(&CfdState::OpenCommitted).unwrap();
        assert_eq!(json, "\"OpenCommitted\"");
        let json = serde_json::to_string(&CfdState::MustRefund).unwrap();
        assert_eq!(json, "\"MustRefund\"");
        let json = serde_json::to_string(&CfdState::Refunded).unwrap();
        assert_eq!(json, "\"Refunded\"");
        let json = serde_json::to_string(&CfdState::SetupFailed).unwrap();
        assert_eq!(json, "\"SetupFailed\"");
    }
}
