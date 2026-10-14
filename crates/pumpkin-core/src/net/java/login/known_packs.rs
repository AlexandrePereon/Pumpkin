#[allow(clippy::wildcard_imports)]
use super::*;

use pumpkin_protocol::java::client::config::CPluginMessage;
use tracing::warn;

use crate::net::java::loader::fabric;

impl PendingConnection {
    pub async fn handle_known_packs(&mut self, server: &Server) {
        // Fabric's registry sync goes first. Tag packets reference registry
        // entries by integer id, so the client has to agree on those ids before
        // anything that uses them arrives.
        match fabric::extension_registry_sync_payload() {
            Ok(Some(payload)) => {
                self.send_packet_now(&CPluginMessage::new(
                    fabric::REGISTRY_SYNC_CHANNEL,
                    &payload,
                ))
                .await;
            }
            // No extension block or item installed, so there are no ids to dictate.
            Ok(None) => {}
            Err(error) => warn!("Could not build the Fabric registry sync payload: {error}"),
        }

        let version = CURRENT_MC_VERSION;
        let registry = pumpkin_data::registry::Registry::get_synced(version);
        for reg in &registry {
            if reg.registry_id == "minecraft:damage_type" {
                let merged = server
                    .datapack_manager
                    .merge_damage_type_entries(&reg.registry_entries);
                self.send_packet_now(&CRegistryData::new(&reg.registry_id, &merged))
                    .await;
                continue;
            }
            let merged = server
                .datapack_manager
                .merge_registry_entries(&reg.registry_id, &reg.registry_entries);
            self.send_packet_now(&CRegistryData::new(&reg.registry_id, &merged))
                .await;
        }
        let mut tags = Vec::new();
        for &key in pumpkin_data::tag::RegistryKey::NETWORK_KEYS {
            if pumpkin_data::tag::get_registry_key_tags(version, key)
                .is_some_and(|map| !map.is_empty())
            {
                tags.push(key);
            }
        }
        self.send_packet_now(&CUpdateTags::new(&tags)).await;
        self.send_packet_now(&CFinishConfig).await;
    }
}
