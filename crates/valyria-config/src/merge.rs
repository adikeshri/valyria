//! Deep-merge of TOML layers, later layers winning key-by-key (not
//! section-by-section — setting one field in `[network]` must not blow
//! away sibling fields set by an earlier layer), with per-leaf origin
//! tracking as each layer is applied.

use toml::Value;

use crate::origin::{ConfigOrigin, OriginMap};

#[derive(Debug)]
pub struct LayeredConfig {
    pub merged: Value,
    pub origins: OriginMap,
}

impl LayeredConfig {
    pub fn new() -> Self {
        Self {
            merged: Value::Table(Default::default()),
            origins: OriginMap::new(),
        }
    }

    pub fn apply_layer(&mut self, layer: Value, source: ConfigOrigin) {
        merge_into(&mut self.merged, &layer, source, &mut self.origins, "");
    }
}

impl Default for LayeredConfig {
    fn default() -> Self {
        Self::new()
    }
}

fn merge_into(
    base: &mut Value,
    overlay: &Value,
    source: ConfigOrigin,
    origins: &mut OriginMap,
    prefix: &str,
) {
    let Value::Table(overlay_table) = overlay else {
        return; // a non-table overlay at the root is not a valid layer; ignore
    };

    if !matches!(base, Value::Table(_)) {
        *base = Value::Table(Default::default());
    }
    let Value::Table(base_table) = base else {
        unreachable!("just normalized to a table")
    };

    for (key, overlay_value) in overlay_table {
        let child_prefix = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };

        let both_tables = matches!(overlay_value, Value::Table(_))
            && matches!(base_table.get(key), Some(Value::Table(_)));

        if both_tables {
            let existing = base_table.get_mut(key).expect("checked above");
            merge_into(existing, overlay_value, source, origins, &child_prefix);
        } else {
            base_table.insert(key.clone(), overlay_value.clone());
            record_leaf_origins(overlay_value, source, origins, &child_prefix);
        }
    }
}

fn record_leaf_origins(value: &Value, source: ConfigOrigin, origins: &mut OriginMap, prefix: &str) {
    match value {
        Value::Table(t) => {
            for (k, v) in t {
                let child = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                record_leaf_origins(v, source, origins, &child);
            }
        }
        _ => {
            origins.insert(prefix.to_string(), source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn later_layer_overrides_specific_leaf_only() {
        let mut layered = LayeredConfig::new();
        layered.apply_layer(
            parse("[network]\ninternet = \"denied\"\ncredentials = \"denied\"\n"),
            ConfigOrigin::Default,
        );
        layered.apply_layer(
            parse("[network]\ninternet = \"controlled\"\n"),
            ConfigOrigin::Workspace,
        );

        let network = layered.merged.get("network").unwrap();
        assert_eq!(
            network.get("internet").unwrap().as_str(),
            Some("controlled")
        );
        // sibling untouched by the later, narrower layer
        assert_eq!(network.get("credentials").unwrap().as_str(), Some("denied"));
    }

    #[test]
    fn origin_reflects_the_last_layer_to_set_each_leaf() {
        let mut layered = LayeredConfig::new();
        layered.apply_layer(
            parse("[permission]\nmode = \"assisted\"\n"),
            ConfigOrigin::Default,
        );
        assert_eq!(
            layered.origins.get("permission.mode"),
            Some(&ConfigOrigin::Default)
        );

        layered.apply_layer(
            parse("[permission]\nmode = \"manual\"\n"),
            ConfigOrigin::Env,
        );
        assert_eq!(
            layered.origins.get("permission.mode"),
            Some(&ConfigOrigin::Env)
        );
    }

    #[test]
    fn empty_layer_changes_nothing() {
        let mut layered = LayeredConfig::new();
        layered.apply_layer(
            parse("[permission]\nmode = \"manual\"\n"),
            ConfigOrigin::Default,
        );
        let before = layered.merged.clone();
        layered.apply_layer(Value::Table(Default::default()), ConfigOrigin::Env);
        assert_eq!(layered.merged, before);
    }
}
