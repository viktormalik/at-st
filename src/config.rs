extern crate yaml_rust;

use crate::analyses::*;
use crate::{Test, TestCase, TestCasesRequirement, DEFAULT_TEST_TIMEOUT};
use log::warn;
use std::fs::{read_to_string, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;
use yaml_rust::{Yaml, YamlLoader};

/// Project configuration
/// Contains:
///   - compiler information
///   - list of test cases to evaluate the solutions on
///   - list of source analyses to run on the solutions
///   - list of additional scripts to be run on each solution
/// Typically parsed from a YAML file
#[derive(Default)]
pub struct Config {
    pub project_path: PathBuf,
    // Solutions information
    pub excluded_dirs: Vec<String>,

    // Basic information
    pub src_file: String,

    // Compiler information
    pub compiler: Option<String>,
    pub c_flags: Option<String>,
    pub ld_flags: Option<String>,

    // Test execution configuration (ms)
    pub timeout: u64,

    pub tests: Vec<Test>,
    pub analyses: Vec<Box<dyn Analyser>>,
    pub scripts: Vec<PathBuf>,
}

/// Configuration errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("invalid format (should be a YAML dictionary)")]
    InvalidFormat,
    #[error("'{option}' has invalid value ({expected_type} expected)")]
    InvalidOption {
        option: String,
        expected_type: String,
    },
    #[error("'{option}' has invalid value of field '{field}' ({expected_type} expected)")]
    InvalidField {
        option: String,
        field: String,
        expected_type: String,
    },
    #[error("command passed to stdin: {msg}")]
    InvalidCommand { msg: String },
    #[error("'{option}' is missing a mandatory field '{field}'")]
    MissingField { option: String, field: String },
    #[error("{source}")]
    BadFile {
        #[from]
        source: std::io::Error,
    },
    #[error("parsing error: {source}")]
    InvalidYaml {
        #[from]
        source: yaml_rust::ScanError,
    },
}

/// Macro for compact error generation
macro_rules! make_error {
    ( $kind:ident, $( $param:ident: $val: expr ),* ) => {
        ConfigError::$kind {
            $(
                $param: $val.to_string(),
            )*
        }
    };
}

impl Config {
    pub fn from_yaml(yaml_file: &Path, project_path: &Path) -> Result<Self, ConfigError> {
        let mut yaml_str = String::new();
        File::open(project_path.join(yaml_file))?.read_to_string(&mut yaml_str)?;
        let yaml = YamlLoader::load_from_str(&yaml_str)?;

        let config_options = yaml[0].as_hash().ok_or(ConfigError::InvalidFormat)?;

        let mut result = Config {
            project_path: project_path.to_path_buf(),
            // Set mandatory fields here
            src_file: mandatory_field_str(&yaml[0], "config", "source")?,
            // Set default values here
            timeout: DEFAULT_TEST_TIMEOUT,
            ..Default::default()
        };

        for (key, val) in config_options.iter() {
            match key.as_str() {
                // Optional fields
                Some("solutions") => {
                    check_fields(val, "solutions", &vec!["exclude-dirs"])?;
                    result.excluded_dirs =
                        optional_field_vec_str(val, "solutions", "exclude-dirs")?.unwrap_or(vec![])
                }
                Some("compiler") => {
                    check_fields(val, "compiler", &vec!["CC", "CFLAGS", "LDFLAGS"])?;
                    result.compiler = optional_field_str(val, "compiler", "CC")?;
                    result.c_flags = optional_field_str(val, "compiler", "CFLAGS")?;
                    result.ld_flags = optional_field_str(val, "compiler", "LDFLAGS")?;
                }
                Some("test-config") => {
                    check_fields(val, "test-config", &vec!["timeout"])?;
                    if let Some(timeout) = optional_field_u64(val, "test-config", "timeout")? {
                        result.timeout = timeout;
                    }
                }
                Some("analyses") => result.analyses = analyses_from_yaml(val)?,
                Some("tests") => result.tests = tests_from_yaml(val)?,
                Some("scripts") => {
                    result.scripts = optional_field_vec_str(&yaml[0], "config", "scripts")?
                        .unwrap_or(vec![])
                        .iter()
                        .map(|s| project_path.join(s))
                        .collect();
                }
                // Mandatory fields (already set)
                Some("source") => {}
                Some(k) => {
                    warn!("Unsupported config option: {}", k);
                }
                None => {
                    warn!("Invalid config option: {:?}", key);
                }
            };
        }
        result.process()
    }

    fn process(mut self) -> Result<Self, ConfigError> {
        for t in &mut self.tests {
            for tc in &mut t.test_cases {
                if let Some(stdin) = tc.stdin.as_ref() {
                    if stdin.starts_with('<') {
                        // Pass contents of a file to stdin
                        tc.stdin = Some(expand_string_from_file(&stdin, &self.project_path)?);
                    } else if stdin.starts_with("$(") {
                        // Expand a command to stdin
                        tc.stdin = Some(expand_string_from_command(&stdin)?);
                    }
                }
                // If stdout should be compared to contents of a file, read the file
                if let Some(stdout) = tc.stdout.as_ref() {
                    tc.stdout = Some(expand_string_from_file(&stdout, &self.project_path)?);
                }
            }
        }
        Ok(self)
    }
}

fn tests_from_yaml(yaml: &Yaml) -> Result<Vec<Test>, ConfigError> {
    match yaml.as_vec() {
        Some(v) => v
            .iter()
            .map(|test| {
                let test_name = optional_field_str(test, "test", "name")?.unwrap_or_default();
                check_fields(
                    test,
                    &test_name,
                    &vec![
                        "name",
                        "score",
                        "args",
                        "stdin",
                        "stdout",
                        "stderr",
                        "test-cases",
                        "require",
                        "case-insensitive",
                    ],
                )?;

                let test_cases = match test["test-cases"].as_vec() {
                    Some(cases) => cases
                        .iter()
                        .map(|case| test_case_from_yaml(case, &test_name, true))
                        .collect::<Result<Vec<TestCase>, _>>()?,
                    None => vec![test_case_from_yaml(test, &test_name, false)?],
                };
                let requirement = match optional_field_str(test, &test_name, "require")?.as_deref()
                {
                    Some("any") => TestCasesRequirement::ANY,
                    Some("all") => TestCasesRequirement::ALL,
                    Some(_) => Err(make_error!(
                        InvalidOption,
                        option: "expect",
                        expected_type: "\"all\" or \"any\""
                    ))?,
                    _ => TestCasesRequirement::ALL,
                };

                Ok(Test {
                    name: test_name.to_string(),
                    score: mandatory_field_f64(test, &test_name, "score")?,
                    test_cases,
                    requirement,
                })
            })
            .collect(),
        None => Ok(vec![]),
    }
}

fn test_case_from_yaml(
    yaml: &Yaml,
    test_name: &str,
    is_inner_case: bool,
) -> Result<TestCase, ConfigError> {
    if is_inner_case {
        check_fields(
            yaml,
            test_name,
            &vec!["args", "stdin", "stdout", "stderr", "case-insensitive"],
        )?;
    }
    Ok(TestCase {
        args: optional_field_str(yaml, test_name, "args")?
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect(),
        stdin: optional_field_str(yaml, test_name, "stdin")?,
        stdout: optional_field_str(yaml, test_name, "stdout")?,
        stderr: optional_field_str(yaml, test_name, "stderr")?,
        case_insensitive: field_bool(yaml, test_name, "case-insensitive")?,
    })
}

fn analyses_from_yaml(yaml: &Yaml) -> Result<Vec<Box<dyn Analyser>>, ConfigError> {
    let mut result = vec![];
    for analysis in yaml.as_vec().unwrap_or(&vec![]) {
        let analysis_name = mandatory_field_str(analysis, "analysis", "analyser")?;
        let kind = AnalyserKind::from(&analysis_name);
        match &kind {
            AnalyserKind::NoCall => {
                check_analysis_fields(analysis, &analysis_name, &vec!["funs", "penalty"])?;
                result.push(Box::new(NoCallAnalyser::new(
                    mandatory_field_vec_str(analysis, "no-call analyser", "funs")?,
                    mandatory_field_f64(analysis, "no-call analyser", "penalty")?,
                )) as Box<dyn Analyser>);
            }
            AnalyserKind::NoHeader => {
                check_analysis_fields(analysis, &analysis_name, &vec!["header", "penalty"])?;
                result.push(Box::new(NoHeaderAnalyser::new(
                    mandatory_field_str(analysis, "no-header analyser", "header")?,
                    mandatory_field_f64(analysis, "no-header analyser", "penalty")?,
                )) as Box<dyn Analyser>);
            }
            AnalyserKind::NoGlobals => {
                check_analysis_fields(analysis, &analysis_name, &vec!["penalty", "except"])?;
                result.push(Box::new(NoGlobalsAnalyser::new(
                    mandatory_field_f64(analysis, "no-globals", "penalty")?,
                    optional_field_vec_str(analysis, "no-globals", "except")?.unwrap_or(vec![]),
                )) as Box<dyn Analyser>);
            }
            AnalyserKind::Unsupported => {
                warn!(
                    "Configuration contains an unsupported analysis \'{}\'",
                    analysis_name
                );
            }
        }
    }
    Ok(result)
}

/// Check if `yaml` is a YAML dictionary (hash) and that it does not contain any keys
/// except those given in `fields`. If an extra key is found, emits a warning.
fn check_fields(yaml: &Yaml, name: &str, fields: &Vec<&str>) -> Result<(), ConfigError> {
    for field in yaml
        .as_hash()
        .ok_or(make_error!(InvalidOption, option: name, expected_type: "dictionary"))?
        .keys()
    {
        let field_name = field.as_str().unwrap_or_default();
        if !fields.contains(&field_name) {
            warn!(
                "Configuration of \'{}\' has unsupported option \'{}\'",
                name, field_name
            );
        }
    }
    Ok(())
}

/// Same as `check_fields`, only specialized for analysis config, which always contains
/// a field "analyser".
fn check_analysis_fields(yaml: &Yaml, name: &str, fields: &Vec<&str>) -> Result<(), ConfigError> {
    let mut analyser_fields = fields.clone();
    analyser_fields.push("analyser");
    let analyser_name = "analyser ".to_string() + name;
    check_fields(yaml, &analyser_name, &analyser_fields)
}

/// Parse `field` from `yaml` as a i64 number.
/// Yields `ConfigError` if the value is not a i64.
/// Returns None if `yaml` does not contain `field`.
fn optional_field_i64(yaml: &Yaml, name: &str, field: &str) -> Result<Option<i64>, ConfigError> {
    match &yaml[field] {
        Yaml::BadValue => Ok(None),
        val => Ok(Some(val.as_i64().ok_or(
            make_error!(InvalidField, option: name, field: field, expected_type: "integer number"),
        )?)),
    }
}

/// Parse `field` from `yaml` as a u32 number.
/// Yields `ConfigError` if the value is not a u32.
/// Returns None if `yaml` does not contain `field`.
fn optional_field_u64(yaml: &Yaml, name: &str, field: &str) -> Result<Option<u64>, ConfigError> {
    match optional_field_i64(yaml, name, field)? {
        Some(n) => match n > 0 {
            true => Ok(Some(n as u64)),
            false => Err(
                make_error!(InvalidField, option: name, field: field, expected_type: "positive int"),
            ),
        },
        None => Ok(None),
    }
}

/// Parse `field` from `yaml` as a f64 number.
/// Yields `ConfigError` if the value is not a f64.
/// Returns None if `yaml` does not contain `field`.
fn optional_field_f64(yaml: &Yaml, name: &str, field: &str) -> Result<Option<f64>, ConfigError> {
    match &yaml[field] {
        Yaml::BadValue => Ok(None),
        val => Ok(Some(val.as_f64().ok_or(
            make_error!(InvalidField, option: name, field: field, expected_type: "float number"),
        )?)),
    }
}

/// Parse `field` from `yaml` as a f64 number.
/// Yields `ConfigError` if `yaml` does not contain `field` or its value is not a f64.
fn mandatory_field_f64(yaml: &Yaml, name: &str, field: &str) -> Result<f64, ConfigError> {
    optional_field_f64(yaml, name, field)?
        .ok_or_else(|| make_error!(MissingField, option: name, field: field))
}

/// Parse `field` from `yaml` as a string.
/// Yields `ConfigError` if the value is not a string.
/// Returns None if `yaml` does not contain `field`.
fn optional_field_str(yaml: &Yaml, name: &str, field: &str) -> Result<Option<String>, ConfigError> {
    match &yaml[field] {
        Yaml::BadValue => Ok(None),
        val => Ok(Some(val.as_str().map(String::from).ok_or(
            make_error!(InvalidField, option: name, field: field, expected_type: "string"),
        )?)),
    }
}

/// Parse `field` from `yaml` as a string.
/// Yields `ConfigError` if `yaml` does not contain `field` or its value is not a string.
fn mandatory_field_str(yaml: &Yaml, name: &str, field: &str) -> Result<String, ConfigError> {
    optional_field_str(yaml, name, field)?
        .ok_or_else(|| make_error!(MissingField, option: name, field: field))
}

/// Parse `field` from `yaml` as a vector of strings.
/// Yields `ConfigError` if the value is not a vector of strings.
/// Returns None if `yaml` does not contain `field`.
fn optional_field_vec_str(
    yaml: &Yaml,
    name: &str,
    field: &str,
) -> Result<Option<Vec<String>>, ConfigError> {
    match &yaml[field] {
        Yaml::BadValue => Ok(None),
        val => Ok(Some(
            val.as_vec()
                .ok_or(make_error!(
                    InvalidField,
                    option: name,
                    field: field,
                    expected_type: "list of strings"))?
                .iter()
                .map(|s| {
                    s.as_str().map(String::from).ok_or(make_error!(
                            InvalidField,
                            option: name,
                            field: field,
                            expected_type: "list of strings"))
                })
                .collect::<Result<Vec<String>, ConfigError>>()?,
        )),
    }
}

/// Parse `field` from `yaml` as a vector of string.
/// Yields `ConfigError` if `yaml` does not contain `field`
/// or its value is not a vector of strings.
fn mandatory_field_vec_str(
    yaml: &Yaml,
    name: &str,
    field: &str,
) -> Result<Vec<String>, ConfigError> {
    optional_field_vec_str(yaml, name, field)?
        .ok_or_else(|| make_error!(MissingField, option: name, field: field))
}

/// Parse `field` from `yaml` as a boolean.
/// Yields `ConfigError` if the value is not a bool.
/// Returns false if `yaml` does not contain `field`.
fn field_bool(yaml: &Yaml, name: &str, field: &str) -> Result<bool, ConfigError> {
    match &yaml[field] {
        Yaml::BadValue => Ok(false),
        val => Ok(val
            .as_bool()
            .ok_or(make_error!(InvalidField, option: name, field: field, expected_type: "bool"))?),
    }
}

/// If `string` starts with '<', interpret the rest as the name of a file inside `project_path`,
/// read the file, and return the contents. Otherwise return the original `string`.
/// If the file does not exist, pass the error along.
fn expand_string_from_file(string: &str, project_path: &Path) -> Result<String, std::io::Error> {
    if string.starts_with('<') {
        let file = project_path.join(&string.trim()[1..]);
        return read_to_string(file.as_path());
    }
    Ok(string.to_string())
}

/// If `string` has form "$(shell command)", execute the command and return its stdout.
/// Otherwise return the original `string`.
/// If the command fails to execute, an error is returned.
fn expand_string_from_command(string: &str) -> Result<String, ConfigError> {
    if !string.starts_with("$(") {
        return Ok(string.to_string());
    }

    if !string.ends_with(')') {
        return Err(make_error!(InvalidCommand, msg: "missing trailing \')\'"));
    }

    let cmd: Vec<&str> = string[2..string.len() - 1].split(' ').collect();
    if cmd.len() == 0 || cmd.len() == 1 && cmd[0].trim().is_empty() {
        return Err(make_error!(InvalidCommand, msg: "empty command"));
    }

    let output = Command::new(&cmd[0]).args(&cmd[1..]).output()?;
    Ok(String::from_utf8(output.stdout)
        .map_err(|e| make_error!(InvalidCommand, msg: format!("{}", e)))?)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_mandatory_str_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: value }").unwrap();
        let f = mandatory_field_str(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert_eq!(f.unwrap(), "value");
    }

    #[test]
    fn parse_mandatory_str_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: value }").unwrap();
        let err = mandatory_field_str(&yaml[0]["option"], "option", "other_field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::MissingField { .. }));
    }

    #[test]
    fn parse_mandatory_str_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: 123 }").unwrap();
        let err = mandatory_field_str(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_optional_str_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: value }").unwrap();
        let f = optional_field_str(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert!(f.as_ref().unwrap().is_some());
        assert_eq!(f.unwrap().unwrap(), "value");
    }

    #[test]
    fn parse_optional_str_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: value }").unwrap();
        let f = optional_field_str(&yaml[0]["option"], "option", "other_field");
        assert!(f.is_ok());
        assert!(f.unwrap().is_none());
    }

    #[test]
    fn parse_optional_str_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: 123 }").unwrap();
        let err = optional_field_str(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_mandatory_f64_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: 1.0 }").unwrap();
        let f = mandatory_field_f64(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert_eq!(f.unwrap(), 1.0);
    }

    #[test]
    fn parse_mandatory_f64_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: 1.0 }").unwrap();
        let err = mandatory_field_f64(&yaml[0]["option"], "option", "other_field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::MissingField { .. }));
    }

    #[test]
    fn parse_mandatory_f64_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: string }").unwrap();
        let err = mandatory_field_f64(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_optional_i64_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: 1 }").unwrap();
        let f = optional_field_i64(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert!(f.as_ref().unwrap().is_some());
        assert_eq!(f.unwrap().unwrap(), 1);
    }

    #[test]
    fn parse_optional_i64_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: 1 }").unwrap();
        let f = optional_field_i64(&yaml[0]["option"], "option", "other_field");
        assert!(f.is_ok());
        assert!(f.unwrap().is_none());
    }

    #[test]
    fn parse_optional_i64_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: 1.0 }").unwrap();
        let err = optional_field_i64(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_optional_u64_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: 1 }").unwrap();
        let f = optional_field_u64(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert!(f.as_ref().unwrap().is_some());
        assert_eq!(f.unwrap().unwrap(), 1);
    }

    #[test]
    fn parse_optional_u64_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: 1 }").unwrap();
        let f = optional_field_u64(&yaml[0]["option"], "option", "other_field");
        assert!(f.is_ok());
        assert!(f.unwrap().is_none());
    }

    #[test]
    fn parse_optional_u64_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: -1 }").unwrap();
        let err = optional_field_u64(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_optional_f64_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: 1.0 }").unwrap();
        let f = optional_field_f64(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert!(f.as_ref().unwrap().is_some());
        assert_eq!(f.unwrap().unwrap(), 1.0);
    }

    #[test]
    fn parse_optional_f64_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: 1.0 }").unwrap();
        let f = optional_field_f64(&yaml[0]["option"], "option", "other_field");
        assert!(f.is_ok());
        assert!(f.unwrap().is_none());
    }

    #[test]
    fn parse_optional_f64_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: string }").unwrap();
        let err = optional_field_f64(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_mandatory_vec_str_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: [ value1, value2 ] }").unwrap();
        let f = mandatory_field_vec_str(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert_eq!(f.unwrap(), vec!["value1", "value2"]);
    }

    #[test]
    fn parse_mandatory_vec_str_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: [ value1, value2 ] }").unwrap();
        let err = mandatory_field_vec_str(&yaml[0]["option"], "option", "other_field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::MissingField { .. }));
    }

    #[test]
    fn parse_mandatory_vec_str_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: value }").unwrap();
        let err = mandatory_field_vec_str(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_optional_vec_str_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: [ value1, value2 ] }").unwrap();
        let f = optional_field_vec_str(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert!(f.as_ref().unwrap().is_some());
        assert_eq!(f.unwrap().unwrap(), vec!["value1", "value2"]);
    }

    #[test]
    fn parse_optional_vec_str_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: [ value1, value2 ] }").unwrap();
        let f = optional_field_vec_str(&yaml[0]["option"], "option", "other_field");
        assert!(f.is_ok());
        assert!(f.unwrap().is_none());
    }

    #[test]
    fn parse_optional_vec_str_invalid() {
        let yaml = YamlLoader::load_from_str("option: { field: value }").unwrap();
        let err = optional_field_vec_str(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn parse_bool_ok() {
        let yaml = YamlLoader::load_from_str("option: { field: true }").unwrap();
        let f = field_bool(&yaml[0]["option"], "option", "field");
        assert!(f.is_ok());
        assert_eq!(f.unwrap(), true);
    }

    #[test]
    fn parse_bool_missing() {
        let yaml = YamlLoader::load_from_str("option: { field: true }").unwrap();
        let f = field_bool(&yaml[0]["option"], "option", "other_field");
        assert!(f.is_ok());
        assert_eq!(f.unwrap(), false);
    }

    #[test]
    fn parse_bool_err() {
        let yaml = YamlLoader::load_from_str("option: { field: \"true\"}").unwrap();
        let err = field_bool(&yaml[0]["option"], "option", "field");
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), ConfigError::InvalidField { .. }));
    }

    #[test]
    fn check_fields_ok() {
        let yaml = YamlLoader::load_from_str("{ field1: val1, field2: val2 }").unwrap();
        let res = check_fields(&yaml[0], "", &vec!["field1", "field2"]);
        assert!(res.is_ok());
    }

    #[test]
    fn expand_string_from_command_ok() {
        let res = expand_string_from_command("$(echo hello)");
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), "hello\n");
    }

    #[test]
    fn expand_string_from_command_empty() {
        let res = expand_string_from_command("$()");
        assert!(res.is_err());
        assert!(matches!(
            res.unwrap_err(),
            ConfigError::InvalidCommand { .. }
        ));
    }

    #[test]
    fn expand_string_from_command_fail() {
        let res = expand_string_from_command("$(nocmd)");
        assert!(res.is_err());
        assert!(matches!(res.unwrap_err(), ConfigError::BadFile { .. }));
    }

    #[test]
    fn tests_from_yaml_single_ok() {
        let yaml = YamlLoader::load_from_str(
            "
- name: test
  score: 1.0
  args: -Wall -Wextra
  stdin: input
  stdout: output",
        )
        .unwrap();
        let res = tests_from_yaml(&yaml[0]);
        assert!(res.is_ok());
        let tests = res.unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "test");
        assert_eq!(tests[0].score, 1.0);
        assert_eq!(tests[0].test_cases.len(), 1);
        assert_eq!(tests[0].test_cases[0].args, vec!["-Wall", "-Wextra"]);
        assert_eq!(tests[0].test_cases[0].stdin, Some("input".to_string()));
        assert_eq!(tests[0].test_cases[0].stdout, Some("output".to_string()));
    }

    #[test]
    fn tests_from_yaml_single_incomplete() {
        let yaml = YamlLoader::load_from_str("[{ score: 1.0 }]").unwrap();
        let res = tests_from_yaml(&yaml[0]);
        assert!(res.is_ok());
        let tests = res.unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "");
        assert_eq!(tests[0].score, 1.0);
        assert_eq!(tests[0].test_cases.len(), 1);
        assert!(tests[0].test_cases[0].args.is_empty());
        assert!(tests[0].test_cases[0].stdin.is_none());
        assert!(tests[0].test_cases[0].stdout.is_none());
    }

    #[test]
    fn tests_from_yaml_multiple() {
        let yaml = YamlLoader::load_from_str(
            "
- name: test
  score: 1.0
  test-cases:
    - args: -Wall -Wextra
      stdin: input
      stdout: output
    - args: -Wall
      stdin: in
      stdout: out
  require: any",
        )
        .unwrap();
        let res = tests_from_yaml(&yaml[0]);
        assert!(res.is_ok());
        let tests = res.unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "test");
        assert_eq!(tests[0].score, 1.0);
        assert_eq!(tests[0].test_cases.len(), 2);
        assert_eq!(tests[0].test_cases[0].args, vec!["-Wall", "-Wextra"]);
        assert_eq!(tests[0].test_cases[0].stdin, Some("input".to_string()));
        assert_eq!(tests[0].test_cases[0].stdout, Some("output".to_string()));
        assert_eq!(tests[0].test_cases[1].args, vec!["-Wall"]);
        assert_eq!(tests[0].test_cases[1].stdin, Some("in".to_string()));
        assert_eq!(tests[0].test_cases[1].stdout, Some("out".to_string()));
    }

    #[test]
    fn tests_from_yaml_missing_field() {
        let yaml = YamlLoader::load_from_str("[{ name: test }]").unwrap();
        let res = tests_from_yaml(&yaml[0]);
        assert!(res.is_err());
        assert!(matches!(res, Err(ConfigError::MissingField { .. })));
    }

    #[test]
    fn analyses_from_yaml_ok() {
        let yaml = YamlLoader::load_from_str(
            "
- analyser: no-call
  funs: [ f1, f2]
  penalty: -1.0
- analyser: no-header
  header: header.h
  penalty: -0.5
- analyser: no-globals
  penalty: -2.0",
        )
        .unwrap();
        let res = analyses_from_yaml(&yaml[0]);
        assert!(res.is_ok());
        let analyses = res.unwrap();
        assert_eq!(analyses.len(), 3);
        assert_eq!(analyses[0].penalty(), -1.0);
        assert_eq!(analyses[1].penalty(), -0.5);
        assert_eq!(analyses[2].penalty(), -2.0);
    }

    #[test]
    fn analyses_from_yaml_invalid() {
        let yaml = YamlLoader::load_from_str("[{ analyser: no-globals }]").unwrap();
        let res = analyses_from_yaml(&yaml[0]);
        assert!(res.is_err());
        assert!(matches!(res, Err(ConfigError::MissingField { .. })));
    }
}
