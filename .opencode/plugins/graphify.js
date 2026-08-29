// graphify OpenCode plugin
// Injects a knowledge graph reminder before bash tool calls that look like
// file discovery / reading (grep/rg/cat/head/tail/find/ls -R/awk/sed/...).
// Fires on EVERY such call (not just once per session) so the assistant is
// nudged toward the graph path whenever it reaches for raw file search.
//
// IMPORTANT: keep the reminder string free of backticks and $(...) constructs.
// The hook prepends `echo "<reminder>" ; <cmd>` to the user's bash command;
// backticks inside the double-quoted echo trigger bash command substitution,
// which both corrupts tool output and silently executes the very graphify
// command we are only suggesting. Plain words render fine in opencode's TUI.
import { existsSync } from "fs";
import { join } from "path";

const REMINDER =
  "[graphify] knowledge graph at graphify-out/. For focused questions, run " +
  "graphify query with your question (scoped subgraph, much smaller than " +
  "GRAPH_REPORT.md) instead of grepping raw files. The graph is current only " +
  "up to the last commit; run graphify update . to refresh it after " +
  "uncommitted or new changes.";

// File discovery / reading commands that warrant a graph nudge.
const DISCOVERY =
  /^(?:grep|egrep|fgrep|rg|rgrep|ag|ack|find|cat|head|tail|awk|sed|less|more|wc|sort|uniq|cut|paste|tr|strings|tree|bat|batcat|od|hexdump|zcat|zgrep|nl)$/;

// Strip leading noise (sudo/env assignments/cd chains) and return the first
// command segment (up to the first ';', '|' or '&').
function firstSegment(cmd) {
  return cmd
    .replace(
      /^(?:(?:sudo|env)\s+|[A-Z_][A-Z0-9_]*=\S*\s+|cd\s+\S+\s*(?:&&|;)\s+)+/i,
      ""
    )
    .split(/[;|&]/)[0]
    .trim();
}

function isFileDiscovery(cmd) {
  const seg = firstSegment(cmd);
  const tool = seg.split(/\s+/)[0].replace(/^\.\//, "");
  if (DISCOVERY.test(tool)) return true;
  // plain `ls` just lists the current directory; only recursive listings
  // (-R, also combined like -alR) count as file discovery.
  if (tool === "ls") return /(?:^|\s)-[a-zA-Z]*R\b/.test(seg);
  return false;
}

export const GraphifyPlugin = async ({ directory }) => {
  return {
    "tool.execute.before": async (input, output) => {
      if (!existsSync(join(directory, "graphify-out", "graph.json"))) return;
      if (input.tool !== "bash") return;

      const cmd = output.args.command || "";
      if (/\bgraphify\b/.test(cmd)) return; // already on the graph path
      if (!isFileDiscovery(cmd)) return;

      // ';' not '&&' — Windows PowerShell 5.1 rejects '&&' as a statement
      // separator, breaking the first bash command of the session (#1646).
      output.args.command = 'echo "' + REMINDER + '" ; ' + cmd;
    },
  };
};