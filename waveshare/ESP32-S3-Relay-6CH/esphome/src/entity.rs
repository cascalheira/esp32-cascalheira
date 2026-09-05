//! Entity registry: what the device exposes to Home Assistant, and how states are encoded.

use std::collections::BTreeMap;

use crate::frame::RawMessage;
use crate::proto::{self, EntityCategory, NumberMode, ServiceArgType};

/// Stable 32-bit key for an object id (FNV-1 like ESPHome's `fnv1_hash`).
pub fn fnv1_hash(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h = h.wrapping_mul(16777619) ^ b as u32;
    }
    h
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Bool(bool),
    Float(f32),
    Text(String),
}

/// Common fields for every entity.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub object_id: String,
    pub name: String,
    pub icon: String,
    pub device_class: String,
    pub category: EntityCategory,
    pub disabled_by_default: bool,
}

impl Meta {
    pub fn new(object_id: &str, name: &str) -> Meta {
        Meta { object_id: object_id.into(), name: name.into(), ..Default::default() }
    }
    pub fn icon(mut self, icon: &str) -> Meta {
        self.icon = icon.into();
        self
    }
    pub fn device_class(mut self, dc: &str) -> Meta {
        self.device_class = dc.into();
        self
    }
    pub fn config(mut self) -> Meta {
        self.category = EntityCategory::Config;
        self
    }
    pub fn diagnostic(mut self) -> Meta {
        self.category = EntityCategory::Diagnostic;
        self
    }
}

#[derive(Debug, Clone)]
pub struct NumberSpec {
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub unit: String,
    pub mode: NumberMode,
}

#[derive(Debug, Clone)]
pub struct SensorSpec {
    pub unit: String,
    pub accuracy_decimals: i32,
    pub state_class: proto::SensorStateClass,
}

#[derive(Debug, Clone)]
pub struct ServiceArg {
    pub name: String,
    pub ty: ServiceArgType,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Switch { assumed_state: bool },
    Number(NumberSpec),
    Select { options: Vec<String> },
    BinarySensor,
    Sensor(SensorSpec),
    TextSensor,
    Button,
    /// User-defined service (`esphome.<device>_<name>` action in HA). Has no state.
    Service { args: Vec<ServiceArg> },
}

#[derive(Debug, Clone)]
pub struct Entity {
    pub key: u32,
    pub meta: Meta,
    pub kind: Kind,
}

impl Entity {
    pub fn has_state(&self) -> bool {
        !matches!(self.kind, Kind::Button | Kind::Service { .. })
    }

    /// The `ListEntities*Response` describing this entity.
    #[allow(deprecated)]
    pub fn list_message(&self) -> RawMessage {
        let m = &self.meta;
        let cat = m.category as i32;
        match &self.kind {
            Kind::Switch { assumed_state } => RawMessage::encode(&proto::ListEntitiesSwitchResponse {
                object_id: m.object_id.clone(),
                key: self.key,
                name: m.name.clone(),
                icon: m.icon.clone(),
                assumed_state: *assumed_state,
                disabled_by_default: m.disabled_by_default,
                entity_category: cat,
                device_class: m.device_class.clone(),
                device_id: 0,
            }),
            Kind::Number(n) => RawMessage::encode(&proto::ListEntitiesNumberResponse {
                object_id: m.object_id.clone(),
                key: self.key,
                name: m.name.clone(),
                icon: m.icon.clone(),
                min_value: n.min,
                max_value: n.max,
                step: n.step,
                disabled_by_default: m.disabled_by_default,
                entity_category: cat,
                unit_of_measurement: n.unit.clone(),
                mode: n.mode as i32,
                device_class: m.device_class.clone(),
                device_id: 0,
            }),
            Kind::Select { options } => RawMessage::encode(&proto::ListEntitiesSelectResponse {
                object_id: m.object_id.clone(),
                key: self.key,
                name: m.name.clone(),
                icon: m.icon.clone(),
                options: options.clone(),
                disabled_by_default: m.disabled_by_default,
                entity_category: cat,
                device_id: 0,
            }),
            Kind::BinarySensor => RawMessage::encode(&proto::ListEntitiesBinarySensorResponse {
                object_id: m.object_id.clone(),
                key: self.key,
                name: m.name.clone(),
                device_class: m.device_class.clone(),
                is_status_binary_sensor: false,
                disabled_by_default: m.disabled_by_default,
                icon: m.icon.clone(),
                entity_category: cat,
                device_id: 0,
            }),
            Kind::Sensor(s) => RawMessage::encode(&proto::ListEntitiesSensorResponse {
                object_id: m.object_id.clone(),
                key: self.key,
                name: m.name.clone(),
                icon: m.icon.clone(),
                unit_of_measurement: s.unit.clone(),
                accuracy_decimals: s.accuracy_decimals,
                force_update: false,
                device_class: m.device_class.clone(),
                state_class: s.state_class as i32,
                legacy_last_reset_type: 0,
                disabled_by_default: m.disabled_by_default,
                entity_category: cat,
                device_id: 0,
            }),
            Kind::TextSensor => RawMessage::encode(&proto::ListEntitiesTextSensorResponse {
                object_id: m.object_id.clone(),
                key: self.key,
                name: m.name.clone(),
                icon: m.icon.clone(),
                disabled_by_default: m.disabled_by_default,
                entity_category: cat,
                device_class: m.device_class.clone(),
                device_id: 0,
            }),
            Kind::Button => RawMessage::encode(&proto::ListEntitiesButtonResponse {
                object_id: m.object_id.clone(),
                key: self.key,
                name: m.name.clone(),
                icon: m.icon.clone(),
                disabled_by_default: m.disabled_by_default,
                entity_category: cat,
                device_class: m.device_class.clone(),
                device_id: 0,
            }),
            Kind::Service { args } => RawMessage::encode(&proto::ListEntitiesServicesResponse {
                name: m.object_id.clone(),
                key: self.key,
                args: args
                    .iter()
                    .map(|a| proto::ListEntitiesServicesArgument {
                        name: a.name.clone(),
                        r#type: a.ty as i32,
                        description: String::new(),
                        example: String::new(),
                    })
                    .collect(),
                supports_response: 0,
                description: m.name.clone(),
            }),
        }
    }

    /// The `*StateResponse` for this entity carrying `state`, or `None` if the state type
    /// does not fit the entity (caller bug) or the entity is stateless.
    pub fn state_message(&self, state: &State) -> Option<RawMessage> {
        let key = self.key;
        Some(match (&self.kind, state) {
            (Kind::Switch { .. }, State::Bool(b)) => {
                RawMessage::encode(&proto::SwitchStateResponse { key, state: *b, device_id: 0 })
            }
            (Kind::BinarySensor, State::Bool(b)) => RawMessage::encode(&proto::BinarySensorStateResponse {
                key,
                state: *b,
                missing_state: false,
                device_id: 0,
            }),
            (Kind::Number(_), State::Float(f)) => RawMessage::encode(&proto::NumberStateResponse {
                key,
                state: *f,
                missing_state: false,
                device_id: 0,
            }),
            (Kind::Sensor(_), State::Float(f)) => RawMessage::encode(&proto::SensorStateResponse {
                key,
                state: *f,
                missing_state: false,
                device_id: 0,
            }),
            (Kind::Select { .. }, State::Text(t)) => RawMessage::encode(&proto::SelectStateResponse {
                key,
                state: t.clone(),
                missing_state: false,
                device_id: 0,
            }),
            (Kind::TextSensor, State::Text(t)) => RawMessage::encode(&proto::TextSensorStateResponse {
                key,
                state: t.clone(),
                missing_state: false,
                device_id: 0,
            }),
            _ => return None,
        })
    }
}

/// All entities of the device, in list order. Keys are derived from object ids and must be unique.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    entities: Vec<Entity>,
    by_key: BTreeMap<u32, usize>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Register an entity and return its key. Panics on a duplicate object id.
    pub fn add(&mut self, meta: Meta, kind: Kind) -> u32 {
        let key = fnv1_hash(&meta.object_id);
        assert!(!self.by_key.contains_key(&key), "duplicate entity object_id {:?}", meta.object_id);
        self.by_key.insert(key, self.entities.len());
        self.entities.push(Entity { key, meta, kind });
        key
    }

    pub fn switch(&mut self, meta: Meta) -> u32 {
        self.add(meta, Kind::Switch { assumed_state: false })
    }

    pub fn number(&mut self, meta: Meta, min: f32, max: f32, step: f32, unit: &str, mode: NumberMode) -> u32 {
        self.add(meta, Kind::Number(NumberSpec { min, max, step, unit: unit.into(), mode }))
    }

    pub fn select(&mut self, meta: Meta, options: &[&str]) -> u32 {
        self.add(meta, Kind::Select { options: options.iter().map(|s| s.to_string()).collect() })
    }

    pub fn binary_sensor(&mut self, meta: Meta) -> u32 {
        self.add(meta, Kind::BinarySensor)
    }

    pub fn sensor(&mut self, meta: Meta, unit: &str, accuracy_decimals: i32, state_class: proto::SensorStateClass) -> u32 {
        self.add(meta, Kind::Sensor(SensorSpec { unit: unit.into(), accuracy_decimals, state_class }))
    }

    pub fn text_sensor(&mut self, meta: Meta) -> u32 {
        self.add(meta, Kind::TextSensor)
    }

    pub fn button(&mut self, meta: Meta) -> u32 {
        self.add(meta, Kind::Button)
    }

    /// `name` becomes the action `esphome.<device>_<name>` in HA.
    pub fn service(&mut self, name: &str, args: &[(&str, ServiceArgType)]) -> u32 {
        let args = args.iter().map(|(n, t)| ServiceArg { name: n.to_string(), ty: *t }).collect();
        self.add(Meta::new(name, name), Kind::Service { args })
    }

    pub fn get(&self, key: u32) -> Option<&Entity> {
        self.by_key.get(&key).map(|&i| &self.entities[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entity> {
        self.entities.iter()
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ApiMessage;

    #[test]
    fn keys_and_messages() {
        let mut r = Registry::new();
        let sw = r.switch(Meta::new("relay_1", "Relay 1"));
        let num = r.number(Meta::new("max_on_1", "Max on 1").config(), 0.0, 1440.0, 1.0, "min", NumberMode::Box);
        let svc = r.service("set_schedule", &[("json", ServiceArgType::String)]);
        assert_eq!(sw, fnv1_hash("relay_1"));
        assert_ne!(sw, num);
        assert_eq!(r.len(), 3);
        assert_eq!(r.get(sw).unwrap().list_message().id, proto::ListEntitiesSwitchResponse::ID);
        assert_eq!(r.get(num).unwrap().list_message().id, proto::ListEntitiesNumberResponse::ID);
        let svc_msg = r.get(svc).unwrap().list_message();
        assert_eq!(svc_msg.id, proto::ListEntitiesServicesResponse::ID);
        let decoded: proto::ListEntitiesServicesResponse = svc_msg.decode().unwrap();
        assert_eq!(decoded.args[0].r#type, ServiceArgType::String as i32);
        assert!(!r.get(svc).unwrap().has_state());
        let st = r.get(sw).unwrap().state_message(&State::Bool(true)).unwrap();
        assert_eq!(st.id, proto::SwitchStateResponse::ID);
        assert!(r.get(sw).unwrap().state_message(&State::Float(1.0)).is_none(), "type mismatch");
    }

    #[test]
    #[should_panic(expected = "duplicate")]
    fn duplicate_object_id_panics() {
        let mut r = Registry::new();
        r.switch(Meta::new("a", "A"));
        r.switch(Meta::new("a", "B"));
    }
}
