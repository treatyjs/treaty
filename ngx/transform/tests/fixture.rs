
use std::{fs::read_to_string, path::PathBuf};

use treaty_authoring::{treaty_authoring, Config};

use swc_core::{
	ecma::transforms::{
        base::resolver,
        testing::test_fixture,
	},
	common::{chain, Mark}
};
use swc_ecma_parser::{EsConfig, Syntax};

#[testing::fixture("tests/fixtures/**/input.ngx")]
fn fixture(input: PathBuf) {
    let dir = input.parent().unwrap();
    let config = read_to_string(dir.join("config.json")).expect("failed to read config.json");
    println!("---- Config -----\n{}", config);
    let _config: Config = serde_json::from_str(&config).unwrap();

    test_fixture(
        Syntax::Es(EsConfig {
            jsx: true,
            ..Default::default()
        }),
        &|t| {
            //
            let fm = t.cm.load_file(&input).unwrap();

            chain!(
                resolver(Mark::new(), Mark::new(), false),
                treaty_authoring(
                    fm.name.clone(),
                    fm.src_hash,
                    _config.clone(),
                    t.comments.clone()
                )
            )
        },
        &input,
        &dir.join("output.js"),
        Default::default(),
    )
}