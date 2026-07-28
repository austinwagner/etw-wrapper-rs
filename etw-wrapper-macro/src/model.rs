//! A resolved model of an ETW manifest.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::eventman::{
    DataType, EventDefinitionType, InstrumentationManifest, LocalizationType, ProviderType,
};

/// The root of a manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub providers: Vec<Provider>,
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub symbol: String,
    /// The provider GUID in `Guid::from_u128` layout.
    pub guid: u128,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub symbol: String,
    pub id: u16,
    pub version: u8,
    pub level: u8,
    pub opcode: u8,
    pub task: u16,
    pub keyword: u64,
    pub channel: u8,
    pub params: Vec<TypeInfo>,
    /// The event's localized message, resolved from the manifest's string table
    /// (English preferred), with `%N` placeholders left as-is. `None` if the event has no
    /// `message` or the reference can't be resolved.
    pub message: Option<String>,
}

/// Parses and resolves an ETW manifest file.
pub fn load(path: impl AsRef<Path>) -> Result<Manifest> {
    let path = path.as_ref();
    let f = File::open(path).with_context(|| format!("opening manifest {}", path.display()))?;
    let man: InstrumentationManifest = serde_xml_rs::from_reader(BufReader::new(f))
        .with_context(|| format!("parsing manifest {}", path.display()))?;
    resolve(&man).with_context(|| format!("resolving manifest {}", path.display()))
}

/// Resolves a parsed manifest into a model.
pub fn resolve(man: &InstrumentationManifest) -> Result<Manifest> {
    let strings = build_message_table(man.localization.as_ref());
    let providers = man
        .instrumentation
        .events
        .provider
        .iter()
        .map(|p| resolve_provider(p, &strings))
        .collect::<Result<Vec<_>>>()?;
    Ok(Manifest { providers })
}

/// Builds an ID-to-value map from the manifest's localized string table, choosing a single
/// culture: `en-US` if present, otherwise any English (`en-*`) culture, otherwise the first
/// `<resources>` block declared.
fn build_message_table(loc: Option<&LocalizationType>) -> HashMap<String, String> {
    let Some(loc) = loc else {
        return HashMap::new();
    };

    let chosen = loc
        .resources
        .iter()
        .find(|r| r.culture.eq_ignore_ascii_case("en-US"))
        .or_else(|| {
            loc.resources.iter().find(|r| {
                r.culture
                    .get(..2)
                    .is_some_and(|p| p.eq_ignore_ascii_case("en"))
            })
        })
        .or_else(|| loc.resources.first());

    let mut map = HashMap::new();
    if let Some(res) = chosen
        && let Some(table) = &res.string_table
    {
        for s in &table.string {
            map.insert(s.id.clone(), s.value.clone());
        }
    }
    map
}

/// Resolves an event's `message` attribute to its display string. A `$(string.ID)` reference is
/// looked up in `strings` (unresolved references yield `None`); any other value is treated as a
/// literal message.
fn resolve_message(raw: Option<&String>, strings: &HashMap<String, String>) -> Option<String> {
    let raw = raw?;
    match raw
        .strip_prefix("$(string.")
        .and_then(|s| s.strip_suffix(')'))
    {
        Some(id) => strings.get(id).cloned(),
        None => Some(raw.clone()),
    }
}

#[derive(Debug)]
struct ProviderLookupTables {
    levels: HashMap<String, u8>,
    keywords: HashMap<String, u64>,
    opcodes: HashMap<String, u8>,
    tasks: HashMap<String, u16>,
    channels: HashMap<String, u8>,
    templates: HashMap<String, Vec<TypeInfo>>,
}

fn resolve_provider(prov: &ProviderType, strings: &HashMap<String, String>) -> Result<Provider> {
    let guid = parse_guid(&prov.guid)
        .with_context(|| format!("provider {} has an invalid guid {:?}", prov.name, prov.guid))?;

    let lookups = ProviderLookupTables {
        levels: collect_levels(prov)?,
        keywords: collect_keywords(prov)?,
        opcodes: collect_opcodes(prov)?,
        tasks: collect_tasks(prov)?,
        channels: collect_channels(prov)?,
        templates: collect_templates(prov)?,
    };

    let mut events = Vec::new();
    if let Some(list) = &prov.events {
        for ev in &list.event {
            events.push(resolve_event(ev, &lookups, strings)?);
        }
    }

    Ok(Provider {
        symbol: prov.symbol.clone(),
        guid,
        events,
    })
}

/// Looks up an event attribute's named reference (`what` describes it in errors), defaulting to
/// zero when the attribute is absent.
fn lookup_or_default<T: Copy + Default>(
    map: &HashMap<String, T>,
    name: Option<&String>,
    what: &str,
    symbol: &str,
) -> Result<T> {
    match name {
        Some(name) => map
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("event {symbol} references undefined {what} {name:?}")),
        None => Ok(T::default()),
    }
}

fn resolve_event(
    ev: &EventDefinitionType,
    lookups: &ProviderLookupTables,
    strings: &HashMap<String, String>,
) -> Result<Event> {
    let id: u16 =
        parse_int(&ev.value).with_context(|| format!("event value {:?} is not a u16", ev.value))?;

    let symbol = ev.symbol.clone().unwrap_or_else(|| format!("Event{id}"));

    let version: u8 = match &ev.version {
        Some(v) => {
            parse_int(v).with_context(|| format!("event {symbol} version {v:?} is not a u8"))?
        }
        None => 0,
    };

    let level = lookup_or_default(&lookups.levels, ev.level.as_ref(), "level", &symbol)?;
    let opcode = lookup_or_default(&lookups.opcodes, ev.opcode.as_ref(), "opcode", &symbol)?;
    let task = lookup_or_default(&lookups.tasks, ev.task.as_ref(), "task", &symbol)?;
    let channel = lookup_or_default(&lookups.channels, ev.channel.as_ref(), "channel", &symbol)?;

    // A keyword is a space-separated list of names combined with bitwise OR
    let mut keyword = 0u64;
    if let Some(list) = &ev.keyword {
        for name in list.split_whitespace() {
            keyword |= *lookups
                .keywords
                .get(name)
                .ok_or_else(|| anyhow!("event {symbol} references undefined keyword {name:?}"))?;
        }
    }

    let params = match &ev.template {
        Some(tid) => lookups
            .templates
            .get(tid)
            .ok_or_else(|| anyhow!("event {symbol} references undefined template {tid:?}"))?
            .clone(),
        None => Vec::new(),
    };

    let message = resolve_message(ev.message.as_ref(), strings);

    Ok(Event {
        symbol,
        id,
        version,
        level,
        opcode,
        task,
        keyword,
        channel,
        params,
        message,
    })
}

fn collect_templates(prov: &ProviderType) -> Result<HashMap<String, Vec<TypeInfo>>> {
    let mut map = HashMap::new();
    if let Some(list) = &prov.templates {
        for t in &list.template {
            let types =
                template_data_to_types(&t.data).with_context(|| format!("template {}", t.tid))?;
            map.insert(t.tid.clone(), types);
        }
    }
    Ok(map)
}

fn collect_levels(prov: &ProviderType) -> Result<HashMap<String, u8>> {
    // Built-in levels
    let mut map: HashMap<String, u8> = [
        ("win:LogAlways", 0u8),
        ("win:Critical", 1),
        ("win:Error", 2),
        ("win:Warning", 3),
        ("win:Informational", 4),
        ("win:Verbose", 5),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), *v))
    .collect();

    if let Some(list) = &prov.levels {
        for l in &list.level {
            let value = parse_int::<u8>(&l.value)
                .with_context(|| format!("level {} value {:?}", l.name, l.value))?;
            map.insert(l.name.clone(), value);
        }
    }
    Ok(map)
}

fn collect_keywords(prov: &ProviderType) -> Result<HashMap<String, u64>> {
    let mut map: HashMap<String, u64> = HashMap::new();
    if let Some(list) = &prov.keywords {
        for k in &list.keyword {
            let mask = parse_int::<u64>(&k.mask)
                .with_context(|| format!("keyword {} mask {:?}", k.name, k.mask))?;
            map.insert(k.name.clone(), mask);
        }
    }
    Ok(map)
}

fn collect_opcodes(prov: &ProviderType) -> Result<HashMap<String, u8>> {
    // Built-in opcodes
    let mut map: HashMap<String, u8> = [
        ("win:Info", 0u8),
        ("win:Start", 1),
        ("win:Stop", 2),
        ("win:DC_Start", 3),
        ("win:DC_Stop", 4),
        ("win:Extension", 5),
        ("win:Reply", 6),
        ("win:Resume", 7),
        ("win:Suspend", 8),
        ("win:Send", 9),
        ("win:Receive", 240),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), *v))
    .collect();

    if let Some(list) = &prov.opcodes {
        for o in &list.opcode {
            let value = parse_int::<u8>(&o.value)
                .with_context(|| format!("opcode {} value {:?}", o.name, o.value))?;
            map.insert(o.name.clone(), value);
        }
    }
    Ok(map)
}

fn collect_tasks(prov: &ProviderType) -> Result<HashMap<String, u16>> {
    let mut map: HashMap<String, u16> = HashMap::new();
    if let Some(list) = &prov.tasks {
        for t in &list.task {
            let value = parse_int::<u16>(&t.value)
                .with_context(|| format!("task {} value {:?}", t.name, t.value))?;
            map.insert(t.name.clone(), value);
        }
    }
    Ok(map)
}

/// Collects channels keyed by `chid`, falling back to `name`, to match how `<event>` elements
/// reference them. An explicit `value` wins; otherwise, custom channels are assigned sequentially
/// from 16 in declaration order, matching the ETW compiler.
fn collect_channels(prov: &ProviderType) -> Result<HashMap<String, u8>> {
    let mut map = HashMap::new();
    let Some(list) = &prov.channels else {
        return Ok(map);
    };

    // Parse explicit value attributes up front, propagating errors instead of silently
    // falling back to automatic numbering, doing this first also lets automatic numbering
    // skip ids already claimed by an explicit value regardless of declaration
    // order, an explicit channel may be declared after an automatically numbered one
    let explicit: Vec<Option<u8>> = list
        .channel
        .iter()
        .map(|c| match &c.value {
            Some(v) => parse_int::<u8>(v)
                .with_context(|| format!("channel {} value {:?}", c.name, v))
                .map(Some),
            None => Ok(None),
        })
        .collect::<Result<_>>()?;

    let mut used: HashSet<u8> = explicit.iter().flatten().copied().collect();
    let mut next = 16u8;
    for (c, value) in list.channel.iter().zip(explicit) {
        let value = match value {
            Some(v) => v,
            None => {
                while used.contains(&next) {
                    next = next
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("too many channels to auto-assign ids"))?;
                }
                let v = next;
                used.insert(v);
                next = next.saturating_add(1);
                v
            }
        };
        let key = c.chid.clone().unwrap_or_else(|| c.name.clone());
        map.insert(key, value);
    }
    Ok(map)
}

/// Parses a GUID.
fn parse_guid(s: &str) -> Result<u128> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        bail!("expected 32 hex digits, found {}", hex.len());
    }
    u128::from_str_radix(&hex, 16).map_err(|e| anyhow!("{e}"))
}

/// Parses an integer that may be decimal or `0x`-prefixed hexadecimal.
fn parse_int<T>(s: &str) -> Result<T>
where
    T: TryFrom<u64>,
    <T as TryFrom<u64>>::Error: std::fmt::Display,
{
    let s = s.trim();
    let raw = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).with_context(|| format!("invalid hex {s:?}"))?
    } else {
        s.parse::<u64>()
            .with_context(|| format!("invalid integer {s:?}"))?
    };
    T::try_from(raw).map_err(|e| anyhow!("value {s:?} out of range: {e}"))
}

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub enum Length {
    Implicit,
    Constant(u32),
    FieldRef(String),
}

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub enum Count {
    Single,
    Constant(u16),
    FieldRef(String),
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Hash)]
pub enum AnsiEncoding {
    /// The provider's ANSI code page, used by the default `xs:string` output type.
    ProviderAnsi,
    /// UTF-8, used by the `win:Utf8`, `win:Json`, and `win:Xml` output types.
    Utf8,
}

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub enum WinType {
    UnicodeString(Length),
    AnsiString(Length, AnsiEncoding),
    Int8,
    UInt8,
    Int16,
    UInt16,
    Int32,
    UInt32,
    Int64,
    UInt64,
    Float,
    Double,
    Boolean,
    Binary(Length),
    Guid,
    Pointer,
    FileTime,
    SystemTime,
    Sid,
    HexInt32,
    HexInt64,
}

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct TypeInfo {
    pub name: String,
    pub win_type: WinType,
    pub count: Count,
}

pub fn template_data_to_types(data: &[DataType]) -> anyhow::Result<Vec<TypeInfo>> {
    fn length_from_string(s: &Option<String>) -> Length {
        match s {
            None => Length::Implicit,
            Some(x) => match x.parse::<u32>() {
                Ok(x) => Length::Constant(x),
                Err(_) => Length::FieldRef(x.to_owned()),
            },
        }
    }

    fn count_from_string(s: &Option<String>) -> anyhow::Result<Count> {
        match s {
            None => Ok(Count::Single),
            Some(raw) if raw.trim().bytes().all(|b| b.is_ascii_digit()) => {
                let value = raw.trim();
                let count = value
                    .parse::<u16>()
                    .with_context(|| format!("array count {raw:?} is not a u16"))?;
                Ok(Count::Constant(count))
            }
            Some(x) => Ok(Count::FieldRef(x.trim().to_owned())),
        }
    }

    fn ansi_encoding(out_type: Option<&str>) -> anyhow::Result<AnsiEncoding> {
        match out_type {
            None | Some("xs:string") => Ok(AnsiEncoding::ProviderAnsi),
            Some("win:Utf8" | "win:Json" | "win:Xml") => Ok(AnsiEncoding::Utf8),
            Some(other) => bail!("unsupported outType {other:?} for win:AnsiString"),
        }
    }

    data.iter()
        .map(|d| {
            let win_type = match d.in_type.as_str() {
                "win:UnicodeString" => WinType::UnicodeString(length_from_string(&d.length)),
                "win:AnsiString" => WinType::AnsiString(
                    length_from_string(&d.length),
                    ansi_encoding(d.out_type.as_deref())
                        .with_context(|| format!("field {}", d.name))?,
                ),
                "win:Int8" => WinType::Int8,
                "win:UInt8" => WinType::UInt8,
                "win:Int16" => WinType::Int16,
                "win:UInt16" => WinType::UInt16,
                "win:Int32" => WinType::Int32,
                "win:UInt32" => WinType::UInt32,
                "win:Int64" => WinType::Int64,
                "win:UInt64" => WinType::UInt64,
                "win:Float" => WinType::Float,
                "win:Double" => WinType::Double,
                "win:Boolean" => WinType::Boolean,
                "win:Binary" => WinType::Binary(length_from_string(&d.length)),
                "win:GUID" => WinType::Guid,
                "win:Pointer" => WinType::Pointer,
                "win:FILETIME" => WinType::FileTime,
                "win:SYSTEMTIME" => WinType::SystemTime,
                "win:SID" => WinType::Sid,
                "win:HexInt32" => WinType::HexInt32,
                "win:HexInt64" => WinType::HexInt64,
                _ => bail!("Unknown type {}", d.in_type),
            };

            Ok(TypeInfo {
                name: d.name.clone(),
                win_type,
                count: count_from_string(&d.count).with_context(|| format!("field {}", d.name))?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WinType;

    fn load_test() -> Manifest {
        load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/manifests/widgetservice.man"
        ))
        .unwrap()
    }

    #[test]
    fn parses_guid_from_string() {
        let g = parse_guid("{8B3A1F42-6C7D-4E9A-9F21-3D5E0A7C1B84}").unwrap();
        assert_eq!(g, 0x8B3A1F42_6C7D_4E9A_9F21_3D5E0A7C1B84);
    }

    #[test]
    fn resolves_hex_event_value_and_version() {
        let xml = r#"
            <instrumentationManifest
                xmlns="http://schemas.microsoft.com/win/2004/08/events"
                xmlns:win="http://manifests.microsoft.com/win/2004/08/windows/events">
              <instrumentation>
                <events>
                  <provider
                      name="Contoso-Hex"
                      guid="{11111111-2222-3333-4444-555555555555}"
                      symbol="PROVIDER_HEX">
                    <events>
                      <event value="0x10" version="0x2" symbol="HEX_EVENT"/>
                    </events>
                  </provider>
                </events>
              </instrumentation>
            </instrumentationManifest>
        "#;
        let manifest: InstrumentationManifest = serde_xml_rs::from_str(xml).unwrap();

        let resolved = resolve(&manifest).unwrap();
        let event = &resolved.providers[0].events[0];

        assert_eq!(event.id, 0x10);
        assert_eq!(event.version, 0x2);
    }

    #[test]
    fn resolves_provider() {
        let m = load_test();
        assert_eq!(m.providers.len(), 1);
        let p = &m.providers[0];
        assert_eq!(p.symbol, "PROVIDER_WIDGETSERVICE");
        assert_eq!(p.guid, 0x8B3A1F42_6C7D_4E9A_9F21_3D5E0A7C1B84);
        assert_eq!(p.events.len(), 2);
    }

    #[test]
    fn resolves_event_descriptor_fields() {
        let p = &load_test().providers[0];
        let started = p
            .events
            .iter()
            .find(|e| e.symbol == "SERVICE_STARTED")
            .unwrap();
        assert_eq!(started.id, 1);
        assert_eq!(started.version, 0);
        assert_eq!(started.level, 4); // win:Informational
        assert_eq!(started.channel, 17);
        assert_eq!(started.params.len(), 3);

        let failed = p
            .events
            .iter()
            .find(|e| e.symbol == "REQUEST_FAILED")
            .unwrap();
        assert_eq!(failed.id, 2);
        assert_eq!(failed.level, 2); // win:Error
        assert_eq!(failed.channel, 16);
    }

    #[test]
    fn resolves_event_messages() {
        let p = &load_test().providers[0];
        let started = p
            .events
            .iter()
            .find(|e| e.symbol == "SERVICE_STARTED")
            .unwrap();
        assert_eq!(
            started.message.as_deref(),
            Some("WidgetService %1 started with %2 workers at %3.")
        );
        let failed = p
            .events
            .iter()
            .find(|e| e.symbol == "REQUEST_FAILED")
            .unwrap();
        assert_eq!(
            failed.message.as_deref(),
            Some("Request %1 failed with status %2 after %3 ms. %4")
        );
    }

    #[test]
    fn picks_english_culture() {
        use crate::eventman::{LocalizationType, ResourcesType, StringTableType, StringType};
        let string = |id: &str, value: &str| StringType {
            id: id.into(),
            value: value.into(),
        };
        let res = |culture: &str, id: &str, value: &str| ResourcesType {
            culture: culture.into(),
            string_table: Some(StringTableType {
                string: vec![string(id, value)],
            }),
        };
        // de-DE first and en-US second, en-US must still win
        let loc = LocalizationType {
            resources: vec![
                res("de-DE", "Event.X", "German"),
                res("en-US", "Event.X", "English"),
            ],
        };
        let table = build_message_table(Some(&loc));
        assert_eq!(table.get("Event.X").map(String::as_str), Some("English"));

        // No English is available, fall back to the first block
        let loc = LocalizationType {
            resources: vec![
                res("fr-FR", "Event.X", "French"),
                res("de-DE", "Event.X", "German"),
            ],
        };
        let table = build_message_table(Some(&loc));
        assert_eq!(table.get("Event.X").map(String::as_str), Some("French"));
    }

    #[test]
    fn resolves_template_param_types() {
        let p = &load_test().providers[0];
        let started = p
            .events
            .iter()
            .find(|e| e.symbol == "SERVICE_STARTED")
            .unwrap();
        let names: Vec<&str> = started.params.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Version", "WorkerCount", "StartTime"]);
        assert_eq!(
            started.params[0].win_type,
            WinType::UnicodeString(Length::Implicit)
        );
        assert_eq!(started.params[1].win_type, WinType::UInt32);
        assert_eq!(started.params[2].win_type, WinType::FileTime);
    }

    #[test]
    fn resolves_ansi_encoding_from_out_type() {
        let field = |out_type: Option<&str>| DataType {
            name: "Text".into(),
            in_type: "win:AnsiString".into(),
            out_type: out_type.map(str::to_owned),
            length: None,
            count: None,
        };

        let default = template_data_to_types(&[field(None)]).unwrap();
        assert_eq!(
            default[0].win_type,
            WinType::AnsiString(Length::Implicit, AnsiEncoding::ProviderAnsi)
        );

        for out_type in ["win:Utf8", "win:Json", "win:Xml"] {
            let resolved = template_data_to_types(&[field(Some(out_type))]).unwrap();
            assert_eq!(
                resolved[0].win_type,
                WinType::AnsiString(Length::Implicit, AnsiEncoding::Utf8)
            );
        }
    }

    #[test]
    fn rejects_unsupported_ansi_out_type() {
        let field = DataType {
            name: "Text".into(),
            in_type: "win:AnsiString".into(),
            out_type: Some("win:HexInt32".into()),
            length: None,
            count: None,
        };

        assert!(template_data_to_types(&[field]).is_err());
    }

    #[test]
    fn resolves_fixed_and_field_reference_array_counts() {
        let field = |name: &str, count: Option<&str>| DataType {
            name: name.into(),
            in_type: "win:UInt32".into(),
            out_type: None,
            length: None,
            count: count.map(str::to_owned),
        };

        let resolved = template_data_to_types(&[
            field("Single", None),
            field("Fixed", Some("3")),
            field("Variable", Some("ElementCount")),
        ])
        .unwrap();

        assert_eq!(resolved[0].count, Count::Single);
        assert_eq!(resolved[1].count, Count::Constant(3));
        assert_eq!(resolved[2].count, Count::FieldRef("ElementCount".into()));
    }

    #[test]
    fn rejects_out_of_range_fixed_array_count() {
        let field = DataType {
            name: "Values".into(),
            in_type: "win:UInt32".into(),
            out_type: None,
            length: None,
            count: Some("65536".into()),
        };

        let err = template_data_to_types(&[field]).unwrap_err();
        assert!(
            format!("{err:#}").contains("array count \"65536\" is not a u16"),
            "{err:#}"
        );
    }
}
