extern crate proc_macro;

use proc_macro::TokenStream;
use std::path::PathBuf;
use syn::{parse_macro_input, LitStr};

/// Generates test cases for a given testing project
///
/// The parameter should be a string with the project directory name,
/// which should be located under tests/projects/.
/// The project name must be a valid Rust identifier.
///
/// The expected directory structure:
///   <project-name>/
///     <solution-1>/
///     <solution-2>/
///     ...
///     config.yaml
///     expected-results
///
/// Generates one test for each solution in the project. The test evaluates
/// the solution and compares the obtained result with the expected result.
///
/// Expected results are specified in 'expected-results' which has the form:
///   <solution-1>: <expected-score>
///   <solution-2>: <expected-score>
///   ...
#[proc_macro]
pub fn generate_tests(input: TokenStream) -> TokenStream {
    let project = parse_macro_input!(input as LitStr).value();
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("projects")
        .join(project.clone());

    let expected = project_path.join("expected-scores");
    let solutions: Vec<String> = std::fs::read_to_string(expected)
        .expect("Error opening \"expected-scores\"")
        .lines()
        .map(|l| l.split(":").nth(0).unwrap().to_string())
        .collect();

    let mut tests = format!("mod {} {{\n", project);
    for solution in solutions {
        tests += &format!(
            "#[test]
            fn test_{}() {{
                let solution = \"{}\";
                let project_path = std::path::PathBuf::from(\"{}\");
                let config_file = std::path::PathBuf::from(\"config.yaml\");

                let expected = std::fs::read_to_string(project_path.join(\"expected-scores\"))
                    .expect(\"Error opening expected-scores\")
                    .lines()
                    .find(|l| l.starts_with(solution)).unwrap()
                    .split(':').nth(1).unwrap()
                    .trim()
                    .parse::<f64>().unwrap();

                let res = atst::run(&project_path, &config_file, solution, 1);

                assert!(res.is_ok());
                assert!(res.as_ref().unwrap().contains_key(solution));
                assert_eq!(*res.as_ref().unwrap().get(solution).unwrap(), expected);
            }}\n",
            solution,
            solution,
            project_path.to_str().unwrap()
        );
    }
    tests += "}\n";

    tests.parse().unwrap()
}
