//! `VALYRIA_*` environment variable overlay (§4.3).
//!
//! Convention: `VALYRIA_<SECTION>_<FIELD>` maps to the dotted path
//! `section.field`, splitting on every underscore after the prefix. This
//! only works unambiguously because settings field names are single words
//! (`mode`, `internet`, `format`, ...) — a field name that itself needed an
//! underscore would collide with the nesting separator, so the settings
//! schema deliberately avoids that rather than adding a `__` escape.

use toml::Value;

const PREFIX: &str = "VALYRIA_";

pub fn env_layer<I>(vars: I) -> Value
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut root = toml::map::Map::new();

    for (key, value) in vars {
        let Some(rest) = key.strip_prefix(PREFIX) else {
            continue;
        };
        let segments: Vec<String> = rest
            .split('_')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect();
        if segments.is_empty() {
            continue;
        }
        insert_path(&mut root, &segments, parse_scalar(&value));
    }

    Value::Table(root)
}

fn insert_path(table: &mut toml::map::Map<String, Value>, segments: &[String], value: Value) {
    match segments {
        [] => {}
        [last] => {
            table.insert(last.clone(), value);
        }
        [head, tail @ ..] => {
            let entry = table
                .entry(head.clone())
                .or_insert_with(|| Value::Table(Default::default()));
            if !matches!(entry, Value::Table(_)) {
                *entry = Value::Table(Default::default());
            }
            if let Value::Table(inner) = entry {
                insert_path(inner, tail, value);
            }
        }
    }
}

fn parse_scalar(raw: &str) -> Value {
    if let Ok(b) = raw.parse::<bool>() {
        return Value::Boolean(b);
    }
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Integer(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return Value::Float(f);
    }
    Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_nested_key_from_env_var_name() {
        let layer = env_layer([("VALYRIA_PERMISSION_MODE".to_string(), "manual".to_string())]);
        assert_eq!(
            layer
                .get("permission")
                .unwrap()
                .get("mode")
                .unwrap()
                .as_str(),
            Some("manual")
        );
    }

    #[test]
    fn ignores_vars_without_the_prefix() {
        let layer = env_layer([("PATH".to_string(), "/usr/bin".to_string())]);
        assert_eq!(layer, Value::Table(Default::default()));
    }

    #[test]
    fn parses_scalars_by_shape() {
        let layer = env_layer([
            ("VALYRIA_A_B".to_string(), "true".to_string()),
            ("VALYRIA_A_C".to_string(), "42".to_string()),
            ("VALYRIA_A_D".to_string(), "3.5".to_string()),
            ("VALYRIA_A_E".to_string(), "hello".to_string()),
        ]);
        let a = layer.get("a").unwrap();
        assert_eq!(a.get("b").unwrap().as_bool(), Some(true));
        assert_eq!(a.get("c").unwrap().as_integer(), Some(42));
        assert_eq!(a.get("d").unwrap().as_float(), Some(3.5));
        assert_eq!(a.get("e").unwrap().as_str(), Some("hello"));
    }

    #[test]
    fn multiple_vars_under_the_same_section_coexist() {
        let layer = env_layer([
            (
                "VALYRIA_NETWORK_INTERNET".to_string(),
                "controlled".to_string(),
            ),
            (
                "VALYRIA_NETWORK_CREDENTIALS".to_string(),
                "denied".to_string(),
            ),
        ]);
        let network = layer.get("network").unwrap();
        assert_eq!(
            network.get("internet").unwrap().as_str(),
            Some("controlled")
        );
        assert_eq!(network.get("credentials").unwrap().as_str(), Some("denied"));
    }
}
