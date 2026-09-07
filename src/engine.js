/**
 * Hero Siege Economy & Item Valuation Engine
 * Evaluates Satanics, Heroics, Angelics, Unholies, Sets, Runes, and Runewords.
 */

export const VERDICTS = {
  INSTANT_PRICE_CHECK: {
    code: 'INSTANT_PRICE_CHECK',
    tier: 1,
    label: 'ALWAYS PRICE CHECK',
    icon: '🔴',
    colorHex: '#ef4444',
    badgeClass: 'badge-danger',
    action: 'Keep & Price Check on Marketplace / Discord / In-game Bazaar immediately.',
  },
  CONDITIONAL_CHECK: {
    code: 'CONDITIONAL_CHECK',
    tier: 2,
    label: 'PRICE CHECK IF GOOD ROLL',
    icon: '🟡',
    colorHex: '#f59e0b',
    badgeClass: 'badge-warning',
    action: 'Inspect Quality %, Sockets, and key roll values before listing or dusting.',
  },
  RARE_EXCEPTION_DUST: {
    code: 'RARE_EXCEPTION_DUST',
    tier: 3,
    label: 'PROSPECT / DUST (UNLESS GOD ROLL)',
    icon: '🔵',
    colorHex: '#3b82f6',
    badgeClass: 'badge-info',
    action: 'Default: Prospect into Satanic Crystals. Only check if Quality ≥ 110% or Max Sockets.',
  },
  DUST_PROSPECT: {
    code: 'DUST_PROSPECT',
    tier: 4,
    label: 'INSTANT DUST / VENDOR',
    icon: '⚪',
    colorHex: '#9ca3af',
    badgeClass: 'badge-secondary',
    action: 'Prospect into Satanic Crystal fragments or vendor. Zero market liquidity.',
  },
};

export const HIGH_VALUE_STATS = {
  all_skills: { weight: 15, label: '+All Skills', isMajor: true },
  all_resistances: { weight: 8, label: '+All Resistances', isMajor: true },
  max_all_resistances: { weight: 12, label: '+Max All Resistances', isMajor: true },
  damage_mitigation: { weight: 10, label: 'Damage Mitigation %', isMajor: true },
  physical_damage_reduction: { weight: 8, label: 'Physical Damage Reduction %', isMajor: true },
  magic_find: { weight: 9, label: 'Magic Find %', isMajor: true },
  increased_attack_speed: { weight: 7, label: 'Attack Speed %', isMajor: false },
  faster_cast_rate: { weight: 7, label: 'Faster Cast Rate %', isMajor: false },
  crit_damage: { weight: 8, label: 'Critical Strike Damage %', isMajor: false },
  crit_chance: { weight: 7, label: 'Critical Strike Chance %', isMajor: false },
  deadly_blow: { weight: 7, label: 'Deadly Blow %', isMajor: false },
  crushing_blow_chance: { weight: 8, label: 'Crushing Blow %', isMajor: false },
  magic_skill_damage: { weight: 6, label: 'Magic Skill Damage %', isMajor: false },
  fire_skill_damage: { weight: 6, label: 'Fire Skill Damage %', isMajor: false },
  cold_skill_damage: { weight: 6, label: 'Cold Skill Damage %', isMajor: false },
  lightning_skill_damage: { weight: 6, label: 'Lightning Skill Damage %', isMajor: false },
  poison_skill_damage: { weight: 6, label: 'Poison Skill Damage %', isMajor: false },
  enhanced_damage: { weight: 6, label: 'Enhanced Damage %', isMajor: false },
};

/**
 * Evaluate an item based on its rarity, grade, slot, stats, sockets, and quality.
 */
export function evaluateItem(item, options = {}) {
  const {
    quality = 100,
    sockets = null,
    seasonPhase = 'mid', // 'early' | 'mid' | 'late'
  } = options;

  if (!item) {
    return {
      verdict: VERDICTS.DUST_PROSPECT,
      score: 0,
      reasons: ['Item not found or invalid.'],
      checklist: [],
      estimatedBracket: 'None',
      qualityMultiplier: 1,
    };
  }

  // Handle Runes
  if (item.id && item.id.startsWith('rune_')) {
    return evaluateRune(item, options);
  }

  // Handle Runewords
  if (item.runes && Array.isArray(item.runes)) {
    return evaluateRuneword(item, options);
  }

  const rarity = (item.rarity || 'common').toLowerCase();
  const grade = (item.grade || 'Ungraded').toUpperCase();
  const slot = (item.slot || 'unknown').toLowerCase();
  const levelReq = item.requiresLevel || 1;
  const implicits = item.implicit || {};
  const maxSockets = item.maxSockets || (slot === 'weapon' ? (item.twoHanded ? 6 : 3) : (slot === 'armor' || slot === 'helmet' || slot === 'shield' ? 3 : 1));

  const reasons = [];
  const checklist = [];
  let baseScore = 0;
  let verdict = VERDICTS.DUST_PROSPECT;
  let estimatedBracket = 'Vendor / Dust';

  // 1. Rarity & Grade Baseline Scoring
  if (rarity === 'angelic') {
    baseScore = 98;
    verdict = VERDICTS.INSTANT_PRICE_CHECK;
    estimatedBracket = 'Ultra High (Ruby/Gold Cap)';
    reasons.push('Angelic rarity: Ultra-rare endgame drop with extreme market demand.');
    checklist.push('Check current listings on Bazaar / Discord; prices often reach high rune / gold caps.');
  } else if (rarity === 'unholy') {
    baseScore = 95;
    verdict = VERDICTS.INSTANT_PRICE_CHECK;
    estimatedBracket = 'Ultra High / High';
    reasons.push('Unholy rarity: Rare corrupted/uber unique with top-tier stats.');
    checklist.push('Verify socket count and quality; ultra-rare collector & endgame staple.');
  } else if (rarity === 'heroic') {
    baseScore = 88;
    verdict = VERDICTS.INSTANT_PRICE_CHECK;
    estimatedBracket = 'High / Very High';
    reasons.push('Heroic rarity: High-tier boss drop unique (always Grade S/SS equivalent).');
    checklist.push('Check socket count and damage rolls; staples in endgame build guides.');
  } else if (rarity === 'satanic' || rarity === 'satanic_set') {
    if (grade === 'SS') {
      baseScore = 85;
      verdict = VERDICTS.INSTANT_PRICE_CHECK;
      estimatedBracket = 'High';
      reasons.push(`Satanic Grade SS (${rarity === 'satanic_set' ? 'Set Piece' : 'Unique'}): Top-tier meta item.`);
      checklist.push('Check roll percentages and sockets; always holds liquid trade value.');
    } else if (grade === 'S') {
      baseScore = 65;
      verdict = VERDICTS.CONDITIONAL_CHECK;
      estimatedBracket = 'Medium to High';
      reasons.push(`Satanic Grade S: Solid meta unique/set. Value depends heavily on quality and sockets.`);
      checklist.push(`Sockets: ${maxSockets > 0 ? `Target ${maxSockets}/${maxSockets} max sockets.` : 'Check jewelry/belt stats.'}`);
      checklist.push('Quality: Satanic Dice roll ≥ 105% significantly boosts value.');
    } else if (grade === 'A') {
      baseScore = 40;
      verdict = VERDICTS.RARE_EXCEPTION_DUST;
      estimatedBracket = 'Low / Satanic Crystals';
      reasons.push('Satanic Grade A: Common unique. Usually prospected into Satanic Crystals.');
      checklist.push('Only check if Quality ≥ 110% or Max Sockets rolled.');
      checklist.push('Early season: May hold value for leveling or Magic Find.');
    } else {
      // Grade B, C, D
      baseScore = 15;
      verdict = VERDICTS.DUST_PROSPECT;
      estimatedBracket = 'Vendor / Satanic Crystals';
      reasons.push(`Satanic Grade ${grade}: Low base tier. 99% used as crafting/crystal dust fodder.`);
      checklist.push('Prospect at Blacksmith for Satanic Crystal Fragments.');
    }
  } else if (rarity === 'relic') {
    baseScore = 50;
    verdict = VERDICTS.CONDITIONAL_CHECK;
    estimatedBracket = 'Low to Medium';
    reasons.push('Relic: Passive buff item. Desirability depends on active meta builds.');
    checklist.push('Check build guides for whether this specific relic is BiS for speed-farming.');
  } else {
    baseScore = 5;
    verdict = VERDICTS.DUST_PROSPECT;
    estimatedBracket = 'Vendor / Ground Trash';
    reasons.push(`${rarity.toUpperCase()} item: Non-satanic gear has negligible trade value unless superior runeword base.`);
  }

  // 2. Modifier & Stat Analysis
  let statScore = 0;
  const notableStats = [];

  for (const [statKey, statRange] of Object.entries(implicits)) {
    if (HIGH_VALUE_STATS[statKey]) {
      const def = HIGH_VALUE_STATS[statKey];
      statScore += def.weight;
      let valText = '';
      if (Array.isArray(statRange)) {
        valText = `[${statRange[0]} - ${statRange[1]}]`;
      } else {
        valText = `${statRange}`;
      }
      notableStats.push(`${def.label}: ${valText}`);
    }
  }

  if (notableStats.length > 0) {
    checklist.push(`Key affixes to verify: ${notableStats.slice(0, 3).join(', ')}`);
  }

  // 3. Quality % Multiplier (Satanic Dice)
  let qualityMultiplier = 1.0;
  if (quality >= 115) {
    qualityMultiplier = 6.0;
    reasons.push('🔥 115% Perfect Quality: God-roll quality multiplier (5x-10x value spike).');
    if (verdict === VERDICTS.RARE_EXCEPTION_DUST || verdict === VERDICTS.CONDITIONAL_CHECK) {
      verdict = VERDICTS.INSTANT_PRICE_CHECK;
      estimatedBracket = 'High / God Roll';
    }
  } else if (quality >= 110) {
    qualityMultiplier = 2.5;
    reasons.push('✨ 110%+ Elite Quality: Substantial value multiplier from Satanic Dice.');
    if (verdict === VERDICTS.RARE_EXCEPTION_DUST) {
      verdict = VERDICTS.CONDITIONAL_CHECK;
      estimatedBracket = 'Medium / Good Roll';
    }
  } else if (quality >= 105) {
    qualityMultiplier = 1.3;
  } else if (quality <= 50) {
    qualityMultiplier = 0.2;
    reasons.push('⚠️ 50% Corrupted Brick: Quality locked at 50% reduces market value drastically.');
  }

  // 4. Socket Analysis
  let socketMultiplier = 1.0;
  const currentSockets = sockets !== null ? sockets : 0;

  if (maxSockets > 1 && sockets !== null) {
    if (currentSockets === maxSockets) {
      socketMultiplier = 1.8;
      reasons.push(`🎯 Max Sockets (${currentSockets}/${maxSockets}): High demand socket count.`);
      if (grade === 'A' && verdict === VERDICTS.RARE_EXCEPTION_DUST) {
        verdict = VERDICTS.CONDITIONAL_CHECK;
      }
    } else if (currentSockets === 0) {
      socketMultiplier = 0.6;
      checklist.push(`Warning: 0 sockets. Needs socket punch/cube recipe to reach full potential.`);
    }
  }

  // 5. Season Phase Calibration
  if (seasonPhase === 'early') {
    if (implicits.magic_find || implicits.all_skills) {
      baseScore += 10;
      reasons.push('Early Season Bonus: Magic Find and +All Skills gear are in peak demand.');
    }
  } else if (seasonPhase === 'late') {
    if (grade === 'A' || grade === 'S') {
      if (quality < 110 && currentSockets < maxSockets) {
        if (verdict === VERDICTS.CONDITIONAL_CHECK) {
          verdict = VERDICTS.RARE_EXCEPTION_DUST;
          reasons.push('Late Season Shift: Mid-tier gear depreciated; market only trades SS/115% rolls.');
        }
      }
    }
  }

  const finalScore = Math.min(100, Math.round((baseScore + statScore * 0.5) * (qualityMultiplier >= 1 ? 1 + (qualityMultiplier - 1) * 0.2 : qualityMultiplier)));

  return {
    item,
    verdict,
    score: finalScore,
    grade,
    rarity,
    slot,
    maxSockets,
    qualityMultiplier,
    socketMultiplier,
    estimatedBracket,
    reasons,
    checklist,
    notableStats,
  };
}

/**
 * Evaluate Runes (Tier 1 to 4)
 */
export function evaluateRune(rune, options = {}) {
  const tier = rune.tier || 1;
  const name = rune.name;

  if (tier >= 4) {
    return {
      item: rune,
      verdict: VERDICTS.INSTANT_PRICE_CHECK,
      score: 100,
      grade: 'High Rune (T4)',
      rarity: 'rune',
      slot: 'socketable',
      estimatedBracket: 'Extreme (Core High Currency)',
      reasons: [`Tier 4 High Rune (${name}): Universal trade currency and top-tier runeword component.`],
      checklist: ['Never vendor or cube down; check current High Rune exchange rates (e.g. 1 T4 = X Rubies/Jah).'],
    };
  }

  if (tier === 3) {
    return {
      item: rune,
      verdict: VERDICTS.INSTANT_PRICE_CHECK,
      score: 85,
      grade: 'High Rune (T3)',
      rarity: 'rune',
      slot: 'socketable',
      estimatedBracket: 'High (Major Currency)',
      reasons: [`Tier 3 High Rune (${name}): Jah/Ber/Sur equivalent high currency. Essential for endgame runewords.`],
      checklist: ['Check current trading ratios; widely used in high-tier runewords.'],
    };
  }

  if (tier === 2) {
    const isHighDemandMid = ['Ist', 'Um', 'Mal', 'Vex'].includes(name);
    return {
      item: rune,
      verdict: VERDICTS.CONDITIONAL_CHECK,
      score: isHighDemandMid ? 65 : 45,
      grade: 'Mid Rune (T2)',
      rarity: 'rune',
      slot: 'socketable',
      estimatedBracket: isHighDemandMid ? 'Medium (High Utility)' : 'Low to Medium',
      reasons: [`Tier 2 Mid Rune (${name}): ${isHighDemandMid ? 'High demand crafting/socket rune (e.g. MF/Resists).' : 'Useful mid-game rune.'}`],
      checklist: ['Save for runewords, socketing gear, or cubing up.'],
    };
  }

  return {
    item: rune,
    verdict: VERDICTS.DUST_PROSPECT,
    score: 20,
    grade: 'Low Rune (T1)',
    rarity: 'rune',
    slot: 'socketable',
    estimatedBracket: 'Negligible / Bulk Crafting',
    reasons: [`Tier 1 Low Rune (${name}): Common drop. Used for early runewords or cubing.`],
    checklist: ['Keep in rune tab or cube up into mid runes.'],
  };
}

/**
 * Evaluate Runewords
 */
export function evaluateRuneword(rw, options = {}) {
  const reqLevel = rw.requiredLevel || 1;
  const runeNames = (rw.runes || []).map((r) => r.replace(/^rune_/, ''));
  const hasHighRune = runeNames.some((r) => ['fawn', 'flo', 'jol', 'nju', 'sur', 'ber', 'jah', 'drax', 'zed', 'qi', 'xo'].includes(r.toLowerCase()));

  if (hasHighRune || reqLevel >= 85) {
    return {
      item: rw,
      verdict: VERDICTS.INSTANT_PRICE_CHECK,
      score: 90,
      grade: 'Endgame Runeword',
      rarity: 'runeword',
      slot: rw.allowedTypes ? rw.allowedTypes.join(', ') : 'gear',
      estimatedBracket: 'High to Ultra High',
      reasons: [`Endgame Runeword (${rw.name}): Requires high runes (${runeNames.join(' + ')}). High base investment.`],
      checklist: ['Requires Superior base item (15% ED/Defense) with exact socket count for maximum market value.'],
    };
  }

  return {
    item: rw,
    verdict: VERDICTS.CONDITIONAL_CHECK,
    score: 55,
    grade: 'Mid/Leveling Runeword',
    rarity: 'runeword',
    slot: rw.allowedTypes ? rw.allowedTypes.join(', ') : 'gear',
    estimatedBracket: 'Medium / Early-Mid Game',
    reasons: [`Leveling/Mid Runeword (${rw.name}): Good progression power.`],
    checklist: ['Ensure base item has matching sockets and decent base stats.'],
  };
}
