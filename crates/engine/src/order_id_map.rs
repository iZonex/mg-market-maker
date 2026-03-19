use mm_common::types::OrderId;
use std::collections::HashMap;

/// Bidirectional mapping between internal UUIDs and exchange-native order IDs.
///
/// Critical for:
/// - Cancelling orders (exchange needs its own ID)
/// - Processing fill events (exchange reports its own ID)
/// - Reconciliation on restart
pub struct OrderIdMap {
    /// Internal UUID → exchange-native string ID.
    to_exchange: HashMap<OrderId, String>,
    /// Exchange-native string ID → internal UUID.
    to_internal: HashMap<String, OrderId>,
    /// Client order ID → internal UUID (for correlation before exchange assigns ID).
    client_to_internal: HashMap<String, OrderId>,
}

impl OrderIdMap {
    pub fn new() -> Self {
        Self {
            to_exchange: HashMap::new(),
            to_internal: HashMap::new(),
            client_to_internal: HashMap::new(),
        }
    }

    /// Register a new mapping after order placement.
    pub fn insert(&mut self, internal_id: OrderId, exchange_id: String) {
        self.to_exchange.insert(internal_id, exchange_id.clone());
        self.to_internal.insert(exchange_id, internal_id);
    }

    /// Register a client order ID for pre-fill correlation.
    pub fn insert_client_id(&mut self, client_oid: String, internal_id: OrderId) {
        self.client_to_internal.insert(client_oid, internal_id);
    }

    /// Look up exchange ID from internal UUID.
    pub fn get_exchange_id(&self, internal_id: &OrderId) -> Option<&String> {
        self.to_exchange.get(internal_id)
    }

    /// Look up internal UUID from exchange ID.
    pub fn get_internal_id(&self, exchange_id: &str) -> Option<&OrderId> {
        self.to_internal.get(exchange_id)
    }

    /// Look up internal UUID from client order ID.
    pub fn get_by_client_id(&self, client_oid: &str) -> Option<&OrderId> {
        self.client_to_internal.get(client_oid)
    }

    /// Remove a mapping (after fill or cancel).
    pub fn remove(&mut self, internal_id: &OrderId) {
        if let Some(exchange_id) = self.to_exchange.remove(internal_id) {
            self.to_internal.remove(&exchange_id);
        }
    }

    /// Number of tracked orders.
    pub fn len(&self) -> usize {
        self.to_exchange.len()
    }

    pub fn is_empty(&self) -> bool {
        self.to_exchange.is_empty()
    }

    /// Clear all mappings.
    pub fn clear(&mut self) {
        self.to_exchange.clear();
        self.to_internal.clear();
        self.client_to_internal.clear();
    }
}

impl Default for OrderIdMap {
    fn default() -> Self {
        Self::new()
    }
}
