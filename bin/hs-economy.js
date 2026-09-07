#!/usr/bin/env node

import { findItem, searchItems, items, runes } from '../src/database.js';
import { evaluateItem, VERDICTS } from '../src/engine.js';
import readline from 'node:readline';

const args = process.argv.slice(2);
const command = args[0] ? args[0].toLowerCase() : 'help';

// Terminal colors
const ANSI = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  cyan: '\x1b[36m',
  white: '\x1b[37m',
  bgRed: '\x1b[41m',
  bgYellow: '\x1b[43m',
  bgBlue: '\x1b[44m',
  bgGray: '\x1b[100m',
};

function formatVerdictBadge(verdict) {
  switch (verdict.code) {
    case 'INSTANT_PRICE_CHECK':
      return `${ANSI.bgRed}${ANSI.white}${ANSI.bold} ${verdict.icon} ${verdict.label} ${ANSI.reset}`;
    case 'CONDITIONAL_CHECK':
      return `${ANSI.bgYellow}${ANSI.white}${ANSI.bold} ${verdict.icon} ${verdict.label} ${ANSI.reset}`;
    case 'RARE_EXCEPTION_DUST':
      return `${ANSI.bgBlue}${ANSI.white}${ANSI.bold} ${verdict.icon} ${verdict.label} ${ANSI.reset}`;
    default:
      return `${ANSI.bgGray}${ANSI.white}${ANSI.bold} ${verdict.icon} ${verdict.label} ${ANSI.reset}`;
  }
}

function parseFlags(cmdArgs) {
  let quality = 100;
  let sockets = null;
  let season = 'mid';
  const cleanTokens = [];

  for (let i = 0; i < cmdArgs.length; i++) {
    const arg = cmdArgs[i];
    if (arg === '-q' || arg === '--quality') {
      quality = parseInt(cmdArgs[++i], 10) || 100;
    } else if (arg === '-s' || arg === '--sockets') {
      sockets = parseInt(cmdArgs[++i], 10);
    } else if (arg === '--season') {
      season = cmdArgs[++i] || 'mid';
    } else {
      cleanTokens.push(arg);
    }
  }

  return { query: cleanTokens.join(' '), quality, sockets, season };
}

function displayEvaluation(result) {
  const item = result.item;
  const v = result.verdict;

  console.log('\n' + '─'.repeat(64));
  console.log(`${ANSI.bold}${item.name}${ANSI.reset} ${ANSI.dim}(${item.baseType || item.slot || 'Item'})${ANSI.reset}`);
  console.log(`Rarity: ${ANSI.cyan}${item.rarity || 'Normal'}${ANSI.reset} | Grade: ${ANSI.yellow}${result.grade || 'N/A'}${ANSI.reset} | Req Lvl: ${ANSI.magenta}${item.requiresLevel || 1}${ANSI.reset}`);
  console.log('─'.repeat(64));

  console.log(`\nVerdict:   ${formatVerdictBadge(v)}`);
  console.log(`Bracket:   ${ANSI.bold}${result.estimatedBracket}${ANSI.reset} (Score: ${result.score}/100)`);
  console.log(`Action:    ${ANSI.dim}${v.action}${ANSI.reset}`);

  if (result.reasons && result.reasons.length > 0) {
    console.log(`\n${ANSI.bold}Evaluation Breakdown:${ANSI.reset}`);
    for (const r of result.reasons) {
      console.log(`  • ${r}`);
    }
  }

  if (result.checklist && result.checklist.length > 0) {
    console.log(`\n${ANSI.bold}Checklist & Roll Targets:${ANSI.reset}`);
    for (const c of result.checklist) {
      console.log(`  🔍 ${c}`);
    }
  }

  if (result.notableStats && result.notableStats.length > 0) {
    console.log(`\n${ANSI.bold}Key Affixes on Item:${ANSI.reset}`);
    for (const stat of result.notableStats) {
      console.log(`  ⚡ ${stat}`);
    }
  }

  console.log('─'.repeat(64) + '\n');
}

function handleCheck(cmdArgs) {
  const { query, quality, sockets, season } = parseFlags(cmdArgs);
  if (!query) {
    console.log(`${ANSI.red}Error: Please provide an item name.${ANSI.reset}`);
    console.log(`Example: hs-economy check "Lucifer's Crown" -q 112 -s 3`);
    return;
  }

  const match = findItem(query);
  if (!match) {
    console.log(`${ANSI.red}No item, rune, or runeword matching "${query}" found.${ANSI.reset}`);
    const suggestions = searchItems(query).slice(0, 5);
    if (suggestions.length > 0) {
      console.log(`\nDid you mean:`);
      for (const s of suggestions) {
        console.log(`  - ${s.name} (${s.rarity} [${s.grade || 'N/A'}])`);
      }
    }
    return;
  }

  const evalResult = evaluateItem(match.data, { quality, sockets, seasonPhase: season });
  displayEvaluation(evalResult);
}

function handleSearch(cmdArgs) {
  const query = cmdArgs.join(' ').trim();
  if (!query) {
    console.log(`${ANSI.red}Error: Please provide a search term.${ANSI.reset}`);
    return;
  }

  const results = searchItems(query).slice(0, 15);
  if (results.length === 0) {
    console.log(`${ANSI.yellow}No items found matching "${query}".${ANSI.reset}`);
    return;
  }

  console.log(`\nFound ${results.length} matching items:\n`);
  console.log(`  ${'Item Name'.padEnd(32)} ${'Rarity'.padEnd(14)} ${'Grade'.padEnd(8)} ${'Slot'.padEnd(12)} Verdict`);
  console.log(`  ${'─'.repeat(30)} ${'─'.repeat(12)} ${'─'.repeat(6)} ${'─'.repeat(10)} ${'─'.repeat(20)}`);

  for (const it of results) {
    const evaluation = evaluateItem(it);
    const badge = `${evaluation.verdict.icon} ${evaluation.verdict.label}`;
    console.log(`  ${(it.name || '').padEnd(32)} ${(it.rarity || '').padEnd(14)} ${(it.grade || '-').padEnd(8)} ${(it.slot || '').padEnd(12)} ${badge}`);
  }
  console.log('');
}

function handleTierlist() {
  console.log(`\n${ANSI.bold}=== HERO SIEGE SATANIC+ ECONOMY TIER LIST ===${ANSI.reset}\n`);

  const ssItems = items.filter((it) => it.grade === 'SS' && ['angelic', 'unholy', 'heroic', 'satanic', 'satanic_set'].includes(it.rarity));
  const sItems = items.filter((it) => it.grade === 'S' && ['heroic', 'satanic', 'satanic_set'].includes(it.rarity));

  console.log(`${ANSI.bgRed}${ANSI.white}${ANSI.bold} TIER 1: SS GRADE (ALWAYS PRICE CHECK) — ${ssItems.length} ITEMS ${ANSI.reset}\n`);
  const ssBySlot = {};
  for (const it of ssItems) {
    const s = it.slot || 'other';
    ssBySlot[s] = ssBySlot[s] || [];
    ssBySlot[s].push(it);
  }

  for (const [slot, slotItems] of Object.entries(ssBySlot)) {
    console.log(`  ${ANSI.cyan}[${slot.toUpperCase()}]${ANSI.reset} (${slotItems.length})`);
    const names = slotItems.map((it) => `${it.name} ${ANSI.dim}(${it.rarity})${ANSI.reset}`).join(', ');
    console.log(`    ${names}\n`);
  }

  console.log(`${ANSI.bgYellow}${ANSI.white}${ANSI.bold} TIER 2: S GRADE (CONDITIONAL PRICE CHECK) — ${sItems.length} ITEMS ${ANSI.reset}\n`);
  const sBySlot = {};
  for (const it of sItems) {
    const s = it.slot || 'other';
    sBySlot[s] = sBySlot[s] || [];
    sBySlot[s].push(it);
  }

  for (const [slot, slotItems] of Object.entries(sBySlot)) {
    console.log(`  ${ANSI.yellow}[${slot.toUpperCase()}]${ANSI.reset} (${slotItems.length})`);
    const names = slotItems.slice(0, 10).map((it) => it.name).join(', ') + (slotItems.length > 10 ? ` ... +${slotItems.length - 10} more` : '');
    console.log(`    ${names}\n`);
  }
}

function handleTriage() {
  console.log(`\n${ANSI.bold}=== HERO SIEGE FAST LOOT TRIAGE REPL ===${ANSI.reset}`);
  console.log(`Type item name as it drops (e.g. 'lucifer', 'bucket', 'hfs', 'harle'). Type 'exit' to quit.\n`);

  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
    prompt: `${ANSI.green}HS-Loot> ${ANSI.reset}`,
  });

  rl.prompt();

  rl.on('line', (line) => {
    const trimmed = line.trim();
    if (trimmed.toLowerCase() === 'exit' || trimmed.toLowerCase() === 'quit') {
      rl.close();
      return;
    }
    if (!trimmed) {
      rl.prompt();
      return;
    }

    const match = findItem(trimmed);
    if (!match) {
      console.log(`  ${ANSI.red}❌ Item not found.${ANSI.reset}`);
    } else {
      const evaluation = evaluateItem(match.data);
      const v = evaluation.verdict;
      console.log(`  ${ANSI.bold}${match.data.name}${ANSI.reset} [${match.data.rarity} ${evaluation.grade}] -> ${formatVerdictBadge(v)} (${evaluation.estimatedBracket})`);
      if (evaluation.checklist.length > 0) {
        console.log(`    ${ANSI.dim}Tip: ${evaluation.checklist[0]}${ANSI.reset}`);
      }
    }
    console.log('');
    rl.prompt();
  });
}

function printHelp() {
  console.log(`
${ANSI.bold}Hero Siege Economy & Price Check Tool${ANSI.reset}

Usage:
  hs-economy check <item-name> [options]    Evaluate item and get price-check verdict
  hs-economy search <query>                 Search items and view quick ratings
  hs-economy tierlist                       Display full SS and S tier breakdown
  hs-economy triage                         Interactive live-drop triage REPL

Options for check:
  -q, --quality <number>     Quality % from Satanic Dice (e.g. 100, 112, 115, 50)
  -s, --sockets <number>     Sockets rolled (e.g. 0 to 6)
  --season <early|mid|late>  Season lifecycle phase (default: mid)

Examples:
  hs-economy check "Lucifer's Crown"
  hs-economy check "Harlequinn's Crest" -q 115 -s 3
  hs-economy check "Jah"
  hs-economy search "sword"
  hs-economy triage
`);
}

// Route command
switch (command) {
  case 'check':
    handleCheck(args.slice(1));
    break;
  case 'search':
    handleSearch(args.slice(1));
    break;
  case 'tierlist':
    handleTierlist();
    break;
  case 'triage':
    handleTriage();
    break;
  case 'help':
  case '--help':
  case '-h':
  default:
    printHelp();
    break;
}
