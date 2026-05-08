/**
 * dBrain — OpenCode plugin adapter
 *
 * Connects OpenCode's event system to the dBrain persistent memory server.
 * dBrain stores identity, entities, structured knowledge (PARA model),
 * and conversation history across all sessions and machines.
 *
 * Flow:
 *   OpenCode events → this plugin → HTTP calls → dBrain server → SQLite
 *
 * dBrain runs as a standalone HTTP server on port 7878. It survives
 * OpenCode restarts and can serve multiple OpenCode instances at once.
 *
 * The MCP connection is configured in opencode.json (type: remote).
 * This plugin handles:
 *   1. Health-check + auto-start of the dBrain process
 *   2. System prompt injection with memory protocol instructions
 *   3. Conversation logging on session events
 *   4. Compaction hooks to preserve context across resets
 */

import type { Plugin } from '@opencode-ai/plugin';
import { truncate } from './utils.js';

// ─── Configuration ───────────────────────────────────────────────────────────

const DBRAIN_PORT = parseInt(process.env.DBRAIN_PORT ?? '7878');
const DBRAIN_URL = `http://127.0.0.1:${DBRAIN_PORT}`;
const DBRAIN_DATA = process.env.DBRAIN_DATA || `${process.env.HOME}/.dbrain`;
const DBRAIN_TOKEN = process.env.DBRAIN_TOKEN ?? '';

// dBrain MCP tool names — skip these in tool-call tracking
const DBRAIN_TOOLS = new Set([
  'wake_up',
  'recall',
  'remember',
  'log',
  'get_entity',
  'list_entities',
  'create_entity',
  'bump',
  'overview',
]);

// ─── Memory Protocol Instructions ────────────────────────────────────────────

const MEMORY_INSTRUCTIONS = `## dBrain — Persistent Memory Protocol

You have a persistent AI brain connected via MCP (dBrain). It stores your identity, user knowledge, entities with structured facts (PARA model), and conversation history. This memory persists across all sessions, machines, and AI clients.

### WHEN TO SEARCH (mandatory — not optional)

Call \`recall\` BEFORE answering any question about:
- The user (preferences, name, context, history)
- Their projects, people they know, or systems they use
- Past conversations or decisions
- Anything the user references as previously discussed

Also search PROACTIVELY when:
- The user's FIRST message in a session mentions a topic — search for prior context
- You're about to say "I don't know" about the user — ALWAYS search first
- The user mentions a person, project, or system — check if an entity exists

Search strategy:
1. Call \`recall\` with the user's question or topic keywords
2. \`recall\` returns BOTH search results AND your identity documents
3. Use \`get_entity\` for deep dives on specific entities found in results
4. Call \`bump\` on any fact you actively use to answer — this keeps it hot

### WHEN TO SAVE (mandatory — not optional)

Call \`remember\` IMMEDIATELY when:
- The user shares a preference, opinion, or personal detail
- A decision is made about a project or system
- The user mentions a new person, relationship, or life event
- Something happens the user would want remembered later
- The user explicitly says "remember this" / "recuerda esto"

Format rules for \`remember\`:
- One clear, atomic fact per call
- Be specific: "Prefers TypeScript strict mode" not "Likes TypeScript"
- Include context when relevant: "Switched to Neovim from VSCode in Jan 2025"
- Attach to the correct entity (use \`list_entities\` to find it, or \`create_entity\` first)

### WHEN TO CREATE ENTITIES

Call \`create_entity\` when the user mentions for the first time:
- A new project they're working on
- A person (colleague, friend, family)
- A system or tool they use regularly
- A significant event or milestone

Then immediately \`remember\` the relevant facts about it.

PARA categories:
- \`projects\` — Active work with deadlines or goals
- \`areas\` — Ongoing responsibilities (health, finance, career)
- \`resources\` — Reference material, tools, interests
- \`archives\` — Completed or inactive items

### CONVERSATION LOGGING

Call \`log\` periodically during conversations to store the exchange. This builds the conversation history that makes future \`recall\` searches richer. Include both user and assistant messages. Use a descriptive source like 'opencode-{project}'.

### SESSION START PROTOCOL

At the start of every conversation:
1. Call \`recall\` with the user's first message or topic — this loads your identity AND searches for relevant context
2. If this is your first ever session, call \`wake_up\` instead to get full identity documents
3. Adapt your behavior based on the identity and soul documents returned

### AFTER COMPACTION

If you see a message about compaction or context reset:
1. Call \`recall\` with a broad query about the current topic to recover context
2. Call \`overview\` if you need the full picture of what's stored
3. Continue working with the recovered context

Do not skip step 1. Without it, you lose awareness of the user's identity and preferences.
`;

// ─── HTTP Client ─────────────────────────────────────────────────────────────

async function dbrainFetch(
  path: string,
  opts: { method?: string; body?: unknown } = {},
): Promise<any> {
  try {
    const headers: Record<string, string> = {};
    if (DBRAIN_TOKEN) headers['Authorization'] = `Bearer ${DBRAIN_TOKEN}`;
    if (opts.body) headers['Content-Type'] = 'application/json';

    const res = await fetch(`${DBRAIN_URL}${path}`, {
      method: opts.method ?? 'GET',
      headers,
      body: opts.body ? JSON.stringify(opts.body) : undefined,
      signal: AbortSignal.timeout(5000),
    });
    return await res.json();
  } catch {
    return null;
  }
}

async function isDbrainRunning(): Promise<boolean> {
  try {
    const res = await fetch(`${DBRAIN_URL}/health`, {
      signal: AbortSignal.timeout(1000),
    });
    return res.ok;
  } catch {
    return false;
  }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

function extractProjectName(directory: string): string {
  try {
    const result = Bun.spawnSync(['git', '-C', directory, 'remote', 'get-url', 'origin']);
    if (result.exitCode === 0) {
      const url = result.stdout?.toString().trim();
      if (url) {
        const name = url.replace(/\.git$/, '').split(/[/:]/).pop();
        if (name) return name;
      }
    }
  } catch {}

  try {
    const result = Bun.spawnSync(['git', '-C', directory, 'rev-parse', '--show-toplevel']);
    if (result.exitCode === 0) {
      const root = result.stdout?.toString().trim();
      if (root) return root.split('/').pop() ?? 'unknown';
    }
  } catch {}

  return directory.split('/').pop() ?? 'unknown';
}

/**
 * Strip <private>...</private> tags before sending to dBrain.
 */
function stripPrivateTags(str: string): string {
  if (!str) return '';
  return str.replace(/<private>[\s\S]*?<\/private>/gi, '[REDACTED]').trim();
}

// ─── Plugin Export ───────────────────────────────────────────────────────────

export const DBrain: Plugin = async (ctx) => {
  const project = extractProjectName(ctx.directory);

  // Track sub-agent sessions to avoid logging their conversations
  const subAgentSessions = new Set<string>();

  // Track active conversation IDs per session for the `log` tool
  const conversationIds = new Map<string, string>();

  // ─── Auto-start ──────────────────────────────────────────────────────────
  const running = await isDbrainRunning();
  if (!running) {
    try {
      // nohup + shell to fully detach — process survives OpenCode closing
      Bun.spawn(['sh', '-c', `nohup npx -y dbrain start ${DBRAIN_DATA} > /dev/null 2>&1 &`], {
        stdout: 'ignore',
        stderr: 'ignore',
        stdin: 'ignore',
      });
      // Retry health-check with backoff
      for (let i = 0; i < 8; i++) {
        await new Promise((r) => setTimeout(r, 1000));
        if (await isDbrainRunning()) break;
      }
    } catch {
      // npx not found or can't start — plugin will silently no-op
    }
  }

  return {
    // ─── Event Listeners ───────────────────────────────────────────

    event: async ({ event }) => {
      if (event.type === 'session.created') {
        const info = (event.properties as any)?.info;
        const sessionId = info?.id;
        const parentID = info?.parentID;
        const title: string = info?.title ?? '';

        const isSubAgent = !!parentID || title.endsWith(' subagent)');
        if (sessionId && isSubAgent) {
          subAgentSessions.add(sessionId);
        }
      }

      if (event.type === 'session.deleted') {
        const info = (event.properties as any)?.info;
        const sessionId = info?.id;
        if (sessionId) {
          subAgentSessions.delete(sessionId);
          conversationIds.delete(sessionId);
        }
      }
    },

    // ─── User Prompt Logging ──────────────────────────────────────
    // Log user messages to dBrain conversation history.
    // This builds the corpus that `recall` searches against.

    'chat.message': async (input, output) => {
      if (subAgentSessions.has(input.sessionID)) return;

      const content = output.parts
        .filter((p) => p.type === 'text')
        .map((p) => (p as any).text ?? '')
        .join('\n')
        .trim();

      if (content.length < 10) return;

      const source = `opencode-${project}`;
      const result = await dbrainFetch('/mcp', {
        method: 'POST',
        body: {
          jsonrpc: '2.0',
          method: 'tools/call',
          params: {
            name: 'log',
            arguments: {
              source,
              conversationId: conversationIds.get(input.sessionID),
              messages: [{ role: 'user', content: stripPrivateTags(truncate(content, 3000)) }],
            },
          },
          id: Date.now(),
        },
      });

      // Store conversation ID for subsequent messages in this session
      if (result?.result?.content?.[0]?.text) {
        try {
          const data = JSON.parse(result.result.content[0].text);
          if (data.conversationId) {
            conversationIds.set(input.sessionID, data.conversationId);
          }
        } catch {}
      }
    },

    // ─── Assistant Response Logging ──────────────────────────────
    // Log assistant replies to dBrain conversation history.
    // experimental.text.complete fires once per completed text part,
    // so we debounce per messageID to send a single log call per turn.

    'experimental.text.complete': async (input, output) => {
      if (subAgentSessions.has(input.sessionID)) return;

      const content = stripPrivateTags(truncate(output.text?.trim() ?? '', 3000));
      if (content.length < 10) return;

      // Only log once per message (first text part that completes)
      const convId = conversationIds.get(input.sessionID);
      if (!convId) return; // no conversation open yet — user message must come first

      await dbrainFetch('/mcp', {
        method: 'POST',
        body: {
          jsonrpc: '2.0',
          method: 'tools/call',
          params: {
            name: 'log',
            arguments: {
              source: `opencode-${project}`,
              conversationId: convId,
              messages: [{ role: 'assistant', content }],
            },
          },
          id: Date.now(),
        },
      });
    },

    // ─── System Prompt: Memory protocol ──────────────────────────
    // Injects MEMORY_INSTRUCTIONS into the system prompt of every message.
    // Ensures the agent ALWAYS knows about dBrain, even after compaction.

    'experimental.chat.system.transform': async (_input, output) => {
      if (output.system.length > 0) {
        output.system[output.system.length - 1] += '\n\n' + MEMORY_INSTRUCTIONS;
      } else {
        output.system.push(MEMORY_INSTRUCTIONS);
      }
    },

    // ─── Compaction Hook ─────────────────────────────────────────
    // When context is compacted, inject instructions for the new agent
    // to recover its identity and context from dBrain.

    'experimental.session.compacting': async (_input, output) => {
      output.context.push(
        `CRITICAL INSTRUCTION FOR COMPACTED SUMMARY:\n` +
          `The agent has access to dBrain persistent memory via MCP tools.\n` +
          `You MUST include the following instruction at the TOP of the compacted summary:\n\n` +
          `"FIRST ACTION REQUIRED: Call \`recall\` with a broad query about the current task. ` +
          `This recovers your identity, user preferences, and relevant context from dBrain. ` +
          `Do this BEFORE any other work."\n\n` +
          `This is NOT optional. Without this, the new agent starts without identity or memory.`,
      );
    },
  };
};
