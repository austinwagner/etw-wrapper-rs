//! A minimal subset of `eventman.xsd` used for ETW wrapper generation.

use serde::Deserialize;

pub type InstrumentationManifest = InstrumentationManifestType;

#[derive(Deserialize)]
pub struct InstrumentationManifestType {
    pub instrumentation: InstrumentationType,
    #[serde(default)]
    pub localization: Option<LocalizationType>,
}

/// Contains one `<resources>` block per culture.
#[derive(Deserialize)]
pub struct LocalizationType {
    #[serde(default)]
    pub resources: Vec<ResourcesType>,
}

#[derive(Deserialize)]
pub struct ResourcesType {
    #[serde(default = "fallback_culture", rename = "@culture")]
    pub culture: String,
    #[serde(default, rename = "stringTable")]
    pub string_table: Option<StringTableType>,
}

fn fallback_culture() -> String {
    "##fallback".to_owned()
}

#[derive(Deserialize)]
pub struct StringTableType {
    #[serde(default)]
    pub string: Vec<StringType>,
}

#[derive(Deserialize)]
pub struct StringType {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@value")]
    pub value: String,
}

#[derive(Deserialize)]
pub struct InstrumentationType {
    pub events: EventsType,
}

#[derive(Deserialize)]
pub struct EventsType {
    #[serde(default)]
    pub provider: Vec<ProviderType>,
}

#[derive(Deserialize)]
pub struct ProviderType {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@guid")]
    pub guid: String,
    #[serde(rename = "@symbol")]
    pub symbol: String,
    #[serde(default)]
    pub channels: Option<ChannelListType>,
    #[serde(default)]
    pub levels: Option<LevelListType>,
    #[serde(default)]
    pub keywords: Option<KeywordListType>,
    #[serde(default)]
    pub opcodes: Option<OpcodeListType>,
    #[serde(default)]
    pub tasks: Option<TaskListType>,
    #[serde(default)]
    pub templates: Option<TemplateListType>,
    #[serde(default)]
    pub events: Option<EventDefinitionListType>,
}

#[derive(Deserialize)]
pub struct ChannelListType {
    #[serde(default)]
    pub channel: Vec<ChannelType>,
}

#[derive(Deserialize)]
pub struct ChannelType {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(default, rename = "@chid")]
    pub chid: Option<String>,
    #[serde(default, rename = "@value")]
    pub value: Option<String>,
}

#[derive(Deserialize)]
pub struct LevelListType {
    #[serde(default)]
    pub level: Vec<LevelType>,
}

#[derive(Deserialize)]
pub struct LevelType {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@value")]
    pub value: String,
}

#[derive(Deserialize)]
pub struct KeywordListType {
    #[serde(default)]
    pub keyword: Vec<KeywordType>,
}

#[derive(Deserialize)]
pub struct KeywordType {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@mask")]
    pub mask: String,
}

#[derive(Deserialize)]
pub struct OpcodeListType {
    #[serde(default)]
    pub opcode: Vec<OpcodeType>,
}

#[derive(Deserialize)]
pub struct OpcodeType {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@value")]
    pub value: String,
}

#[derive(Deserialize)]
pub struct TaskListType {
    #[serde(default)]
    pub task: Vec<TaskType>,
}

#[derive(Deserialize)]
pub struct TaskType {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@value")]
    pub value: String,
}

#[derive(Deserialize)]
pub struct TemplateListType {
    #[serde(default)]
    pub template: Vec<TemplateType>,
}

#[derive(Deserialize)]
pub struct TemplateType {
    #[serde(rename = "@tid")]
    pub tid: String,
    #[serde(default)]
    pub data: Vec<DataType>,
}

#[derive(Deserialize)]
pub struct DataType {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@inType")]
    pub in_type: String,
    #[serde(default, rename = "@outType")]
    pub out_type: Option<String>,
    #[serde(default, rename = "@length")]
    pub length: Option<String>,
}

#[derive(Deserialize)]
pub struct EventDefinitionListType {
    #[serde(default)]
    pub event: Vec<EventDefinitionType>,
}

#[derive(Deserialize)]
pub struct EventDefinitionType {
    #[serde(rename = "@value")]
    pub value: String,
    #[serde(default, rename = "@symbol")]
    pub symbol: Option<String>,
    #[serde(default, rename = "@version")]
    pub version: Option<String>,
    #[serde(default, rename = "@channel")]
    pub channel: Option<String>,
    #[serde(default, rename = "@level")]
    pub level: Option<String>,
    #[serde(default, rename = "@opcode")]
    pub opcode: Option<String>,
    #[serde(default, rename = "@task")]
    pub task: Option<String>,
    #[serde(default, rename = "@keyword")]
    pub keyword: Option<String>,
    #[serde(default, rename = "@template")]
    pub template: Option<String>,
    #[serde(default, rename = "@message")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_without_culture_use_schema_fallback() {
        let xml = r#"
            <instrumentationManifest xmlns="http://schemas.microsoft.com/win/2004/08/events">
              <instrumentation><events/></instrumentation>
              <localization>
                <resources>
                  <stringTable><string id="Event.X" value="fallback"/></stringTable>
                </resources>
              </localization>
            </instrumentationManifest>
        "#;

        let manifest: InstrumentationManifest = serde_xml_rs::from_str(xml).unwrap();
        let resources = &manifest.localization.unwrap().resources;
        assert_eq!(resources[0].culture, "##fallback");
    }
}
