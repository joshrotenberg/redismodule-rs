// An example library in JavaScript, built into a component with
// componentize-js (see build.mjs). It provides the same functions as the Rust
// example in ../hello, so the same tests run against both.

import { call, log } from 'redis-module-wasm:host/redis@0.1.0';

let count = 0n;

const functionsByName = {
  // A greeting, as a simple string, and a log line.
  hello(keys, args) {
    const who = args.length > 0 ? decode(args[0]) : 'world';
    log('notice', `hello called for ${who}`);
    return [{ tag: 'simple', val: `hello, ${who}` }];
  },
  // INCR the key twice and reply with both results.
  incr_twice(keys) {
    return [
      { tag: 'array', val: 2 },
      ...call('INCR', [keys[0]]),
      ...call('INCR', [keys[0]]),
    ];
  },
  // SET the key, passing the +OK status through.
  set(keys, args) {
    return call('SET', [keys[0], args[0]]);
  },
  // An array holding a map, a nil and an error.
  nested() {
    return [
      { tag: 'array', val: 3 },
      { tag: 'map', val: 1 },
      { tag: 'simple', val: 'answer' },
      { tag: 'integer', val: 42n },
      { tag: 'nil' },
      { tag: 'error', val: 'ERR an error inside an array' },
    ];
  },
  // An error reply.
  fail() {
    return [{ tag: 'error', val: 'ERR this function always fails' }];
  },
  // Counts the calls this instance has served.
  counter() {
    count += 1n;
    return [{ tag: 'integer', val: count }];
  },
  // Loops until WASM.KILL stops it.
  spin() {
    for (;;) {}
  },
};

function decode(bytes) {
  return new TextDecoder().decode(bytes);
}

// The flags of the functions that have any; the rest may write. WIT flags are
// objects of booleans named in lower camel case.
const flagsByName = {
  hello: { noWrites: true },
  nested: { noWrites: true },
  fail: { noWrites: true },
  counter: { noWrites: true },
  spin: { noWrites: true },
};

export function functions() {
  return Object.keys(functionsByName).map((name) => ({
    name,
    flags: flagsByName[name] ?? {},
  }));
}

export function invoke(name, keys, args) {
  const f = functionsByName[name];
  return f ? f(keys, args) : [{ tag: 'error', val: `ERR unknown function '${name}'` }];
}
