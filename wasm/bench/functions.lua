#!lua name=bench

-- The Lua side of the benchmark: the same functions as guest/src/lib.rs,
-- written the same way.

-- Two INCRs of the key, as an array.
redis.register_function('incr_twice', function(keys)
  return {redis.call('INCR', keys[1]), redis.call('INCR', keys[1])}
end)

-- An array holding a map (RESP2: a flat array), a nil and an error.
redis.register_function('nested', function()
  return {{'answer', 42}, false, {err = 'ERR an error inside an array'}}
end)

-- The Adler-32 checksum of the key's value, one byte at a time.
redis.register_function('adler32', function(keys)
  local value = redis.call('GET', keys[1]) or ''
  local a, b = 1, 0
  for i = 1, #value do
    a = (a + string.byte(value, i)) % 65521
    b = (b + a) % 65521
  end
  return b * 65536 + a
end)

-- The lower median of the integers in the list at the key.
redis.register_function('median', function(keys)
  local items = redis.call('LRANGE', keys[1], 0, -1)
  local numbers = {}
  for i, item in ipairs(items) do
    numbers[i] = tonumber(item)
  end
  table.sort(numbers)
  return numbers[math.floor((#numbers + 1) / 2)]
end)

-- How many items the list at the key has, fetched as median fetches them.
redis.register_function('lrange', function(keys)
  return #redis.call('LRANGE', keys[1], 0, -1)
end)

-- How many primes there are up to n, with a sieve.
redis.register_function('primes', function(keys, args)
  local n = tonumber(args[1])
  local composite = {}
  local count = 0
  for i = 2, n do
    if not composite[i] then
      count = count + 1
      for multiple = i * i, n, i do
        composite[multiple] = true
      end
    end
  end
  return count
end)
