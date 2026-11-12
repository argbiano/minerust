// `minerust prepare`: builds the data the vanilla world generator reads from the
// owner's own server.jar. Nothing of Mojang's ships with minerust.
//
// Most of it is copied straight out of the jar (worldgen JSON, tags, structure
// templates). Block facts and block behaviour come from Horizon's exporters, run
// against the server's classes, and the biome tables from the data generator.

mod survival;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    process::Command,
};
use tracing::info;
use zip::ZipArchive;

pub const VERSION: &str = "26.3";

const EXPORTERS: &[(&str, &str)] = &[
    (
        "VanillaHeightmapOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaHeightmapOracle.java"),
    ),
    (
        "VanillaSimpleBlockOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaSimpleBlockOracle.java"),
    ),
    (
        "VanillaStructureBlockExporter",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaStructureBlockExporter.java"),
    ),
    (
        "VanillaBlockSurvivalOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaBlockSurvivalOracle.java"),
    ),
    (
        "VanillaPlacementOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaPlacementOracle.java"),
    ),
    (
        "VanillaBlockSupportOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaBlockSupportOracle.java"),
    ),
    (
        "VanillaBlockStateCodecOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaBlockStateCodecOracle.java"),
    ),
    (
        "VanillaBlockPropertiesOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaBlockPropertiesOracle.java"),
    ),
    (
        "VanillaCollisionOracle",
        include_str!("../../prepare/exporters/dev/horizonlab/parity/VanillaCollisionOracle.java"),
    ),
];

// (bundle file, folder under data/minecraft/) as GeneratorData::load expects them.
const BUNDLES: &[(&str, &str)] = &[
    ("noises", "worldgen/noise"),
    ("densities", "worldgen/density_function"),
    ("rules", "worldgen/material_rule"),
    ("conditions", "worldgen/material_condition"),
    ("settings", "worldgen/noise_settings"),
    ("carvers", "worldgen/carver"),
    ("biomes", "worldgen/biome"),
    ("world_presets", "worldgen/world_preset"),
    ("features", "worldgen/feature"),
    ("placed_features", "worldgen/placed_feature"),
    ("state_providers", "worldgen/block_state_provider"),
    ("biome_tags", "tags/worldgen/biome"),
    ("feature_tags", "tags/worldgen/feature"),
    ("block_tags", "tags/block"),
    ("fluid_tags", "tags/fluid"),
    ("processor_lists", "worldgen/processor_list"),
    ("template_pools", "worldgen/template_pool"),
    ("structures", "worldgen/structure"),
    ("structure_sets", "worldgen/structure_set"),
];

const BLOCK_FILES: &[&str] = &[
    "block-properties.nbt",
    "cached-collision.nbt",
    "cached-support.nbt",
    "survival-extra-definitions.json",
    "placement-definitions.json",
];

pub fn run(server_jar: &Path, out: &Path) -> Result<()> {
    // The JVMs run inside the scratch folder, so relative paths would point somewhere else.
    let server_jar = &std::path::absolute(server_jar)?;
    let out = &std::path::absolute(out)?;
    let java = Java::find()?;
    let scratch = out.join(".scratch");
    let classpath = scratch.join("classpath");
    let worldgen = out.join("worldgen");
    fs::create_dir_all(&classpath)?;
    fs::create_dir_all(worldgen.join("blocks"))?;

    info!("unpacking {}", server_jar.display());
    let server_classes = unpack_bundle(server_jar, &classpath)?;

    info!("copying worldgen data");
    let mut inner = ZipArchive::new(Cursor::new(fs::read(&server_classes)?))?;
    write_bundles(&mut inner, &worldgen)?;
    copy_structures(&mut inner, &worldgen)?;

    info!("compiling the block exporters");
    let sources = scratch.join("exporters");
    let classes = scratch.join("classes");
    fs::create_dir_all(&classes)?;
    let mut files = Vec::new();
    for (name, text) in EXPORTERS {
        let path = sources.join(format!("dev/horizonlab/parity/{name}.java"));
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, text)?;
        files.push(path);
    }
    let cp = format!("{}/*", classpath.display());
    let mut javac = java.javac();
    javac.args(["--release", "25", "-proc:none", "-encoding", "UTF-8", "-nowarn", "-cp", &cp, "-d"]);
    javac.arg(&classes).args(&files);
    run_command(javac, "javac")?;

    let exporter_cp = format!("{}{}{}", classes.display(), if cfg!(windows) { ";" } else { ":" }, cp);
    let exporter = |main: &str, args: &[&Path]| -> Result<()> {
        info!("running {main}");
        let mut cmd = java.java();
        cmd.current_dir(&scratch).args(["-Xmx1G", "-cp", &exporter_cp]).arg(format!("dev.horizonlab.parity.{main}"));
        cmd.args(args);
        run_command(cmd, main)
    };
    let facts = out.join("facts.json");
    let export = scratch.join("blocks");
    fs::create_dir_all(&export)?;
    exporter("VanillaHeightmapOracle", &[Path::new("export"), &facts])?;
    exporter("VanillaSimpleBlockOracle", &[Path::new("export"), &export])?;
    exporter("VanillaStructureBlockExporter", &[&worldgen.join("blocks/structure-blocks.json")])?;
    for name in BLOCK_FILES {
        fs::copy(export.join(name), worldgen.join("blocks").join(name))
            .with_context(|| format!("exporter left no {name}"))?;
    }
    let read = |name: &str| -> Result<Value> { Ok(serde_json::from_slice(&fs::read(export.join(name))?)?) };
    let rules = survival::bind_definitions(
        &read("survival-classes.json")?,
        &read("survival-placement-classes.json")?,
        &read("survival-support-tags.json")?,
    )?;
    fs::write(worldgen.join("blocks/survival-rules.json"), serde_json::to_vec(&rules)?)?;

    info!("writing block flags");
    write_block_flags(&facts, &worldgen.join("block_flags.json"))?;

    info!("running the data generator for the biome tables");
    let reports = scratch.join("datagen");
    let mut datagen = java.java();
    datagen.current_dir(&scratch).arg("-DbundlerMainClass=net.minecraft.data.Main").arg("-jar").arg(server_jar);
    datagen.arg("--reports").arg("--output").arg(&reports);
    run_command(datagen, "data generator")?;
    let tables = worldgen.join("biome_parameters");
    fs::create_dir_all(&tables)?;
    for entry in fs::read_dir(reports.join("reports/biome_parameters/minecraft"))? {
        let path = entry?.path();
        fs::copy(&path, tables.join(path.file_name().unwrap()))?;
    }

    fs::write(out.join("minerust-data.json"), serde_json::to_vec_pretty(&serde_json::json!({ "version": VERSION }))?)?;
    fs::remove_dir_all(&scratch).ok();
    info!("done: {}", out.display());
    Ok(())
}

/// Checks that `dir` holds data from `prepare` for this version.
pub fn check(dir: &Path) -> Result<()> {
    let marker = dir.join("minerust-data.json");
    let text = fs::read_to_string(&marker).with_context(|| {
        format!("{} not found; run `minerust prepare --server-jar <server.jar>` first", marker.display())
    })?;
    let version: Value = serde_json::from_str(&text)?;
    ensure!(
        version["version"] == VERSION,
        "{} was prepared for Minecraft {}, this server needs {VERSION}; prepare again",
        dir.display(),
        version["version"]
    );
    Ok(())
}

/// Extracts the server and its libraries from the bundler jar; returns the server's own jar.
fn unpack_bundle(server_jar: &Path, dest: &Path) -> Result<PathBuf> {
    let mut jar = ZipArchive::new(fs::File::open(server_jar).with_context(|| server_jar.display().to_string())?)?;
    let mut versions = String::new();
    jar.by_name("META-INF/versions.list")
        .context("not a Minecraft server jar (no META-INF/versions.list)")?
        .read_to_string(&mut versions)?;
    ensure!(
        versions.split_whitespace().any(|f| f == VERSION),
        "this is not the {VERSION} server jar (versions.list: {})",
        versions.trim()
    );

    let mut server = None;
    for i in 0..jar.len() {
        let mut entry = jar.by_index(i)?;
        let name = entry.name()?.into_owned();
        let wanted = name.ends_with(".jar")
            && (name.starts_with("META-INF/libraries/") || name.starts_with(&format!("META-INF/versions/{VERSION}/")));
        if !wanted {
            continue;
        }
        let file = Path::new(&name).file_name().context("bad jar entry")?;
        let path = dest.join(file);
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        fs::write(&path, bytes)?;
        if name.starts_with("META-INF/versions/") {
            server = Some(path);
        }
    }
    server.context("server jar holds no server classes")
}

fn write_bundles(jar: &mut ZipArchive<Cursor<Vec<u8>>>, out: &Path) -> Result<()> {
    let mut names: Vec<String> = jar.file_names().map(|n| n.map(|n| n.into_owned())).collect::<Result<_, _>>()?;
    names.sort();
    for (bundle, folder) in BUNDLES {
        let prefix = format!("data/minecraft/{folder}/");
        let mut resources = Map::new();
        for name in names.iter().filter(|n| n.starts_with(&prefix) && n.ends_with(".json")) {
            let mut text = String::new();
            jar.by_name(name)?.read_to_string(&mut text)?;
            let id = format!("minecraft:{}", &name[prefix.len()..name.len() - 5]);
            resources.insert(id, Value::String(text));
        }
        if resources.is_empty() {
            bail!("server jar has no {folder} resources");
        }
        fs::write(out.join(format!("{bundle}.json")), serde_json::to_vec(&resources)?)?;
    }
    Ok(())
}

fn copy_structures(jar: &mut ZipArchive<Cursor<Vec<u8>>>, out: &Path) -> Result<()> {
    let prefix = "data/minecraft/structure/";
    let names: Vec<String> = jar.file_names().map(|n| n.map(|n| n.into_owned())).collect::<Result<Vec<_>, _>>()?;
    let names: Vec<String> = names.into_iter().filter(|n| n.starts_with(prefix) && n.ends_with(".nbt")).collect();
    for name in names {
        let relative = &name[prefix.len()..];
        ensure!(!relative.split('/').any(|c| c == ".." || c.is_empty()), "odd structure path {name}");
        let path = out.join("structure/minecraft").join(relative);
        fs::create_dir_all(path.parent().unwrap())?;
        let mut bytes = Vec::new();
        jar.by_name(&name)?.read_to_end(&mut bytes)?;
        fs::write(path, bytes)?;
    }
    Ok(())
}

/// `[is_air, has_fluid]` per block state, from columns 1 and 2 of facts.json's rows.
fn write_block_flags(facts: &Path, out: &Path) -> Result<()> {
    let facts: Value = serde_json::from_slice(&fs::read(facts)?)?;
    let rows = facts["states"].as_array().context("facts.json has no states")?;
    let flags: Vec<Value> = rows.iter().map(|row| Value::Array(vec![row[1].clone(), row[2].clone()])).collect();
    fs::write(out, serde_json::to_vec(&flags)?)?;
    Ok(())
}

struct Java {
    bin: Option<PathBuf>,
}

impl Java {
    fn find() -> Result<Self> {
        let bin = std::env::var_os("JAVA_HOME").map(|home| PathBuf::from(home).join("bin"));
        let java = Self { bin };
        let version = java.java().arg("-version").output().context(
            "Java not found. Install a JDK 25 (for example from adoptium.net) and put it on PATH or set JAVA_HOME",
        )?;
        let text = String::from_utf8_lossy(&version.stderr);
        let major = text
            .split('"')
            .nth(1)
            .and_then(|v| v.split(['.', '-', '+']).next())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        ensure!(
            major >= 25,
            "Minecraft {VERSION} needs Java 25 or newer, found: {}",
            text.lines().next().unwrap_or("")
        );
        Ok(java)
    }

    fn tool(&self, name: &str) -> Command {
        match &self.bin {
            Some(bin) => Command::new(bin.join(name)),
            None => Command::new(name),
        }
    }

    fn java(&self) -> Command {
        self.tool("java")
    }

    fn javac(&self) -> Command {
        self.tool("javac")
    }
}

fn run_command(mut cmd: Command, what: &str) -> Result<()> {
    let output = cmd.output().with_context(|| format!("starting {what} (is a JDK installed?)"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let tail: Vec<&str> = stderr.lines().chain(stdout.lines()).rev().take(15).collect();
        bail!("{what} failed ({}):\n{}", output.status, tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}
