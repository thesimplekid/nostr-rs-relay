//! Relay metadata using NIP-11
/// Relay Info
use crate::config;
use serde::{Deserialize, Serialize};

pub const CARGO_PKG_VERSION: Option<&'static str> = option_env!("CARGO_PKG_VERSION");
pub const UNIT: &str = "sats";

/// Limitations of the relay as specified in NIP-111
/// (This nip isn't finalized so may change)
#[derive(Debug, Serialize, Deserialize)]
#[allow(unused)]
pub struct Limitation {
    #[serde(skip_serializing_if = "Option::is_none")]
    payment_required: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
#[allow(unused)]
pub struct Fees {
    #[serde(skip_serializing_if = "Option::is_none")]
    admission: Option<Vec<Fee>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publication: Option<Vec<Fee>>,
}

#[derive(Serialize, Deserialize, Debug)]
#[allow(unused)]
pub struct Fee {
    amount: u64,
    unit: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(unused)]
pub struct RelayInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_nips: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limitation: Option<Limitation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fees: Option<Fees>,
}

impl RelayInfo {
    pub fn new(i: config::Info, p: config::PayToRelay) -> Self {
        let limitations = Limitation {
            payment_required: Some(p.enabled),
        };

        let (payment_url, fees) = if p.enabled {
            let admission_fee = if p.admission_cost > 0 {
                Some(vec![Fee {
                    amount: p.admission_cost,
                    unit: UNIT.to_string(),
                }])
            } else {
                None
            };

            let post_fee = if p.cost_per_event > 0 {
                Some(vec![Fee {
                    amount: p.cost_per_event,
                    unit: UNIT.to_string(),
                }])
            } else {
                None
            };

            let fees = Fees {
                admission: admission_fee,
                publication: post_fee,
            };

            let payment_url = if p.enabled && i.relay_url.is_some() {
                Some(format!(
                    "{}join",
                    i.relay_url.clone().unwrap().replace("ws", "http")
                ))
            } else {
                None
            };
            (payment_url, Some(fees))
        } else {
            (None, None)
        };

        RelayInfo {
            id: i.relay_url,
            name: i.name,
            description: i.description,
            pubkey: i.pubkey,
            contact: i.contact,
            supported_nips: Some(vec![1, 2, 9, 11, 12, 15, 16, 20, 22, 33, 111]),
            software: Some("https://git.sr.ht/~gheartsfield/nostr-rs-relay".to_owned()),
            version: CARGO_PKG_VERSION.map(std::borrow::ToOwned::to_owned),
            limitation: Some(limitations),
            payment_url,
            fees,
        }
    }
}
