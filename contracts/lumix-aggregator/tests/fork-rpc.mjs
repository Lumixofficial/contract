// Read-only RPC transport for local fork tests. No signing or submission methods.
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { createInterface } from 'node:readline';
const path = process.env.LUMIX_FORK_CACHE ?? new URL('fixtures/mainnet-rpc.json', import.meta.url);
const live = process.env.LUMIX_FORK_LIVE === '1';
const cache = existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : { entries: {} };
if (!live && !cache.ledger) throw new Error('Capture fork data first with LUMIX_FORK_LIVE=1');
let id = 0;
async function rpc(method, params = {}) {
  if (!['getNetwork', 'getLatestLedger', 'getLedgerEntries'].includes(method)) throw new Error('Read-only transport');
  const response = await fetch(process.env.STELLAR_RPC_URL ?? 'https://soroban-rpc.mainnet.stellar.gateway.fm', {
    method: 'POST', headers: { 'content-type': 'application/json' }, signal: AbortSignal.timeout(30000),
    body: JSON.stringify({ jsonrpc: '2.0', id: ++id, method, params }),
  });
  if (!response.ok) throw new Error(`RPC HTTP ${response.status}`);
  const data = await response.json();
  if (data.error) throw new Error(`RPC error ${data.error.code}`);
  return data.result;
}
if (live) {
  const network = await rpc('getNetwork');
  if (network.passphrase !== 'Public Global Stellar Network ; September 2015') throw new Error('Wrong fork network');
  cache.ledger = await rpc('getLatestLedger'); cache.timestamp = Math.floor(Date.now() / 1000); cache.capturedAt = new Date().toISOString();
  // Refresh a previously discovered footprint in ONE read for a consistent state.
  const keys = Object.keys(cache.entries);
  if (keys.length > 200) throw new Error('Fork footprint exceeds a single RPC batch');
  if (keys.length) {
    const result = await rpc('getLedgerEntries', { keys });
    cache.entries = Object.fromEntries(keys.map(k => [k, null]));
    for (const entry of result.entries) cache.entries[entry.key] = entry;
    cache.ledger.sequence = result.latestLedger; cache.firstReadLedger = result.latestLedger; cache.lastReadLedger = result.latestLedger;
  }
  writeFileSync(path, JSON.stringify(cache));
}
for await (const line of createInterface({ input: process.stdin })) {
  try {
    const query = JSON.parse(line);
    if (query.method === 'info') { console.log(JSON.stringify({ ledger: cache.ledger, timestamp: cache.timestamp })); continue; }
    const key = query.key;
    if (!Object.hasOwn(cache.entries, key)) {
      if (!live) throw new Error('Key absent from captured fork; recapture in live mode');
      const result = await rpc('getLedgerEntries', { keys: [key] });
      cache.entries[key] = result.entries[0] ?? null;
      cache.firstReadLedger ??= result.latestLedger; cache.lastReadLedger = result.latestLedger;
      writeFileSync(path, JSON.stringify(cache));
    }
    console.log(JSON.stringify({ entry: cache.entries[key] }));
  } catch (error) { console.log(JSON.stringify({ error: error.message })); }
}
