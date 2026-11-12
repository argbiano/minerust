// The command tree sent to Java clients, for tab completion and client-side parsing.
// Only commands the player may use are included, as vanilla does.

use super::codec::Writer;
use crate::{commands::COMMANDS, game::mob::MobKind};

const ROOT: u8 = 0;
const LITERAL: u8 = 1;
const ARGUMENT: u8 = 2;
const EXECUTABLE: u8 = 4;

// Ids from the command_argument_type registry.
const ARG_STRING: i32 = 5;
const ARG_GAMEMODE: i32 = 42;

#[derive(Clone, Copy)]
enum Parser {
    Word,
    Greedy,
    GameMode,
}

struct Node {
    kind: u8,
    name: &'static str,
    parser: Option<Parser>,
    executable: bool,
    children: Vec<usize>,
}

#[derive(Default)]
struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    fn add(&mut self, parent: usize, kind: u8, name: &'static str, parser: Option<Parser>, executable: bool) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node { kind, name, parser, executable, children: Vec::new() });
        self.nodes[parent].children.push(id);
        id
    }

    fn literal(&mut self, parent: usize, name: &'static str, executable: bool) -> usize {
        self.add(parent, LITERAL, name, None, executable)
    }

    fn arg(&mut self, parent: usize, name: &'static str, parser: Parser) -> usize {
        self.add(parent, ARGUMENT, name, Some(parser), true)
    }
}

pub fn commands_packet(level: u8) -> Vec<u8> {
    let mut tree = Tree::default();
    tree.nodes.push(Node { kind: ROOT, name: "", parser: None, executable: false, children: Vec::new() });

    for spec in COMMANDS.iter().filter(|s| level >= s.level) {
        match spec.name {
            "gamemode" => {
                let cmd = tree.literal(0, "gamemode", false);
                let mode = tree.arg(cmd, "gamemode", Parser::GameMode);
                tree.arg(mode, "player", Parser::Word);
            }
            "op" | "deop" => {
                let cmd = tree.literal(0, spec.name, false);
                tree.arg(cmd, "player", Parser::Word);
            }
            "kick" => {
                let cmd = tree.literal(0, "kick", false);
                let player = tree.arg(cmd, "player", Parser::Word);
                tree.arg(player, "reason", Parser::Greedy);
            }
            "whitelist" => {
                let cmd = tree.literal(0, "whitelist", false);
                for sub in ["on", "off", "list", "reload"] {
                    tree.literal(cmd, sub, true);
                }
                for sub in ["add", "remove"] {
                    let s = tree.literal(cmd, sub, false);
                    tree.arg(s, "player", Parser::Word);
                }
            }
            "summon" => {
                let cmd = tree.literal(0, "summon", false);
                for kind in MobKind::ALL {
                    tree.literal(cmd, kind.name(), true);
                }
            }
            "time" => {
                let cmd = tree.literal(0, "time", false);
                tree.literal(cmd, "query", true);
                let set = tree.literal(cmd, "set", false);
                for value in ["day", "noon", "night", "midnight"] {
                    tree.literal(set, value, true);
                }
            }
            "wgen" => {
                let cmd = tree.literal(0, "wgen", false);
                tree.literal(cmd, "stop", true);
                tree.literal(cmd, "status", true);
                // `<radius> [<x> <z>] [cps] [dimension]` is parsed on the server.
                tree.arg(cmd, "args", Parser::Greedy);
            }
            name => {
                tree.literal(0, name, true);
            }
        }
    }

    let mut w = Writer::new();
    w.varint(tree.nodes.len() as i32);
    for node in &tree.nodes {
        w.u8(node.kind | if node.executable { EXECUTABLE } else { 0 });
        w.varint(node.children.len() as i32);
        for &c in &node.children {
            w.varint(c as i32);
        }
        if node.kind != ROOT {
            w.string(node.name);
        }
        match node.parser {
            Some(Parser::Word) => w.varint(ARG_STRING).varint(0),
            Some(Parser::Greedy) => w.varint(ARG_STRING).varint(2),
            Some(Parser::GameMode) => w.varint(ARG_GAMEMODE),
            None => &mut w,
        };
    }
    w.varint(0); // root index
    w.buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ops_only_see_their_commands() {
        let player = commands_packet(0);
        let op = commands_packet(4);
        let has = |buf: &[u8], s: &str| buf.windows(s.len()).any(|w| w == s.as_bytes());
        assert!(has(&player, "help") && has(&player, "list"));
        assert!(!has(&player, "gamemode") && !has(&player, "whitelist"));
        assert!(has(&op, "gamemode") && has(&op, "whitelist") && has(&op, "stop"));
    }
}
