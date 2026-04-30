use std::str::FromStr;

use jsonrpsee::core::Serialize;
use serde::Deserialize;
use strum::{self, IntoEnumIterator};

/// The versions of the Graph Tally JSON-RPC API implemented by this server.
/// The version numbers are independent of the Graph Tally software version. As such, we are
/// enabling the introduction of breaking changes to the Graph Tally library interface without
/// necessarily introducing breaking changes to the JSON-RPC API (or vice versa).
#[derive(
    Clone,
    Debug,
    Eq,
    PartialEq,
    strum::Display,
    strum::EnumString,
    strum::VariantNames,
    strum::EnumIter,
)]
pub enum GraphTallyRpcApiVersion {
    #[strum(serialize = "0.0")]
    V0_0,
}

// We implement our own Serialize and Deserialize traits for `GraphTallyRpcApiVersion` because
// the ones derived by `serde` serialize the enum member names as strings (eg. "V0_0"),
// while we want to serialize them using the variant strings we set through `strum`
// (eg. "0.0").

impl serde::ser::Serialize for GraphTallyRpcApiVersion {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_str())
    }
}

impl<'de> serde::de::Deserialize<'de> for GraphTallyRpcApiVersion {
    fn deserialize<D>(deserializer: D) -> std::result::Result<GraphTallyRpcApiVersion, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        GraphTallyRpcApiVersion::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// List of RPC version numbers for which a deprecation warning has to be issued.
/// This is a very basic approach to deprecation warnings. The most important thing
/// is to have *some* process in place to warn users of breaking changes.
/// NOTE: Make sure to test it when that list becomes non-empty.
pub static GRAPH_TALLY_RPC_API_VERSIONS_DEPRECATED: &[GraphTallyRpcApiVersion] = &[];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GraphTallyRpcApiVersionsInfo {
    pub versions_supported: Vec<GraphTallyRpcApiVersion>,
    pub versions_deprecated: Vec<GraphTallyRpcApiVersion>,
}

pub fn graph_tally_rpc_api_versions_info() -> GraphTallyRpcApiVersionsInfo {
    GraphTallyRpcApiVersionsInfo {
        versions_supported: GraphTallyRpcApiVersion::iter().collect::<Vec<_>>(),
        versions_deprecated: GRAPH_TALLY_RPC_API_VERSIONS_DEPRECATED.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_tally_rpc_api_version_serialize() {
        let version = GraphTallyRpcApiVersion::V0_0;
        let serialized = serde_json::to_string(&version).unwrap();
        assert_eq!(serialized, "\"0.0\"");
    }

    #[test]
    fn test_graph_tally_rpc_api_version_deserialize() {
        let version = GraphTallyRpcApiVersion::V0_0;
        let deserialized: GraphTallyRpcApiVersion = serde_json::from_str("\"0.0\"").unwrap();
        assert_eq!(deserialized, version);
    }
}
