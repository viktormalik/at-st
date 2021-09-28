mod analyses;
mod config;
mod modules;

use config::Config;
use modules::*;
use std::path::{Path, PathBuf};

/// One student task that is to be evaluated
pub struct Solution {
    path: PathBuf,
    src_file: PathBuf,
    obj_file: PathBuf,
    bin_file: PathBuf,

    included: Vec<String>,
    source: String,

    score: f64,
}

impl Solution {
    pub fn new(path: &Path, config: &Config) -> Self {
        let src_file = Path::new(config.src_file.as_ref().unwrap());
        Self {
            path: path.to_path_buf(),
            src_file: src_file.to_path_buf(),
            bin_file: PathBuf::from(src_file.file_stem().unwrap()),
            obj_file: src_file.with_extension("o"),
            included: vec![],
            source: String::new(),
            score: 0.0,
        }
    }
}

/// Single test case for the project
/// Contains test name, test input (args and stdin), expected output, and test score
pub struct TestCase {
    pub name: String,
    pub score: f64,
    pub args: Vec<String>,
    pub stdin: String,
    pub stdout: String,
}

/// Main entry point of the program
/// Runs evaluation of all tests in `path` as defined in `config_file`
pub fn run(path: &PathBuf, config_file: &PathBuf) {
    let config = Config::from_yaml(&config_file, &path);

    // Solutions are sub-directories of the student directory starting with 'x'
    let solutions = path
        .read_dir()
        .expect("Could not read project directory")
        .filter_map(|res| res.ok())
        .filter(|entry| {
            entry.path().is_dir() && entry.file_name().into_string().unwrap().starts_with('x')
        })
        .map(|entry| Solution::new(&entry.path(), &config));

    // Create modules that will be run on each solution
    // Currently used modules:
    //  - compilation
    //  - source parsing
    //  - test cases execution
    //  - source analyses
    //  - custom scripts
    let mut modules: Vec<Box<dyn Module>> = vec![];
    modules.push(Box::new(Compiler::new(&config)));
    modules.push(Box::new(Parser {}));
    modules.push(Box::new(TestExec::new(&config.test_cases)));
    modules.push(Box::new(AnalysesExec::new(&config.analyses)));
    for script in &config.scripts {
        modules.push(Box::new(ScriptExec::new(script)));
    }

    // Evaluation - run all modules on each solution
    for mut solution in solutions {
        print!("{}: ", solution.path.file_name().unwrap().to_str().unwrap());
        for m in &modules {
            m.execute(&mut solution);
        }
        println!("{}", (solution.score * 100.0).round() / 100.0);
    }
}