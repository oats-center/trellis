//! Real package compilation and the server's canonical-evidence compiler.
//!
//! Run `cargo bench -p trellis-idl --bench compile`. Set
//! `TRELLIS_IDL_BENCH_PROJECT` to measure one local project instead of the
//! checked-in platform, Console, and demo packages. File reads and preparation
//! are excluded; owned-input cloning and compiled-graph destruction are included.

use std::{collections::BTreeMap, hint::black_box, path::Path, time::Instant};
use trellis_idl::{
    canonical_package, compile_evidence, compile_project,
    project::{load_sources, read_manifest, PackageManifest},
    CanonicalMode, PackageEvidence, PackageGraph, PackageSourceEvidence, SourceUnit,
};

struct Input {
    manifest: PackageManifest,
    sources: Vec<SourceUnit>,
    dependencies: BTreeMap<String, Input>,
}

impl Input {
    fn load(root: &Path) -> Self {
        let manifest = read_manifest(&root.join("trellis.toml")).unwrap();
        let sources = load_sources(root, &manifest).unwrap();
        let dependencies = manifest
            .dependencies
            .iter()
            .map(|(alias, dependency)| {
                let path = dependency
                    .path
                    .as_ref()
                    .expect("compiler benchmark requires locally available dependencies");
                (alias.clone(), Self::load(&root.join(path)))
            })
            .collect();
        Self {
            manifest,
            sources,
            dependencies,
        }
    }

    fn compile(&self) -> PackageGraph {
        let dependencies = self
            .dependencies
            .iter()
            .map(|(alias, input)| (alias.clone(), input.compile()))
            .collect();
        compile_project(&self.manifest, self.sources.clone(), dependencies).unwrap()
    }
}

fn main() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let projects = std::env::var_os("TRELLIS_IDL_BENCH_PROJECT").map_or_else(
        || {
            [
                "crates/runtime",
                "web",
                "demos/ts/service",
                "demos/rust/service",
            ]
            .into_iter()
            .map(|path| repository.join(path))
            .collect::<Vec<_>>()
        },
        |path| vec![path.into()],
    );
    for project in projects {
        let input = Input::load(&project);
        let graph = input.compile();
        let evidence = PackageEvidence {
            root_package: graph.root().as_str().to_owned(),
            root_digest: graph.root_digest().to_owned(),
            packages: graph
                .packages()
                .iter()
                .map(|(id, package)| PackageSourceEvidence {
                    name: id.as_str().to_owned(),
                    version: package.version().clone(),
                    digest: graph.digest(id).unwrap().to_owned(),
                    source: canonical_package(&graph, id, CanonicalMode::Presentation).unwrap(),
                })
                .collect(),
        };
        assert_eq!(
            compile_evidence(evidence.clone()).unwrap().root_digest(),
            graph.root_digest()
        );
        for phase in ["compile", "evidence"] {
            let mut samples = Vec::new();
            for iteration in 0..23 {
                let started = Instant::now();
                match phase {
                    "compile" => drop(black_box(input.compile())),
                    "evidence" => drop(black_box(compile_evidence(evidence.clone()).unwrap())),
                    _ => unreachable!(),
                }
                if iteration >= 2 {
                    samples.push(started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{} {phase}: n={} median={:.3}ms min={:.3}ms max={:.3}ms",
                project.display(),
                samples.len(),
                samples[samples.len() / 2],
                samples[0],
                samples[samples.len() - 1]
            );
        }
    }
}
