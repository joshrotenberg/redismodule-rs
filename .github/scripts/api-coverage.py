#!/usr/bin/env python3
"""Generate or check the module API coverage table in docs/API-COVERAGE.md."""

import argparse
from collections import defaultdict, deque
from pathlib import Path
import re
import sys
import textwrap

ROOT = Path(__file__).resolve().parents[2]
HEADER = ROOT / "src" / "include" / "redismodule.h"
SRC = ROOT / "src"
# The generated bindings declare every API pointer, so they say nothing about
# coverage.
BINDINGS = SRC / "redisraw"
PROC_MACROS = ROOT / "redismodule-rs-macros" / "src"
CARGO = ROOT / "Cargo.toml"
DOC = ROOT / "docs" / "API-COVERAGE.md"
BEGIN = "<!-- api-coverage:begin -->"
END = "<!-- api-coverage:end -->"
PREFIX = "RedisModule_"
STATUSES = ["safe", "unsafe", "internal", "none"]
MODES = ["required", "available", "api!", "direct"]
LOOKUPS = {"__required_api": "required", "__available_api": "available"}

# The header groups these under "bigredis extensions"; no Redis OSS source
# defines them.
ENTERPRISE = {
    "SwapPrefetchKey", "IsKeyInRam", "BigModuleRegister", "BigWriteBufferBudgetInit",
    "BigWriteBufferBudgetRelease", "SetOpenFilesReservation", "BigGetDbPath",
    "BigRegisterDb", "BigRegisterDbAddCf", "BigUnregisterDb",
}

# Classifications checked by hand where reachability from public items gives
# the wrong answer: API -> (status, reason).
OVERRIDES = {
    "Alloc": ("internal", "`RedisAlloc`, the global allocator, and the allocator check in "
              "`redis_module!` use it; no wrapper allocates through it."),
    "Free": ("internal", "`RedisAlloc` frees with it, and wrappers free buffers Redis returned "
             "(`RedisBuffer`, the key array of `get_command_keys`); no wrapper frees through it."),
    "GetApi": ("internal", "the header's inline `RedisModule_InitAPI`, exported by "
               "`src/redismodule.c`, resolves every API pointer through it."),
    "SetModuleAttribs": ("internal", "the header's inline `RedisModule_Init`, which "
                         "`redis_module!` calls through `src/redismodule.c`, calls it."),
    "IsModuleNameBusy": ("internal", "the header's inline `RedisModule_Init`, which "
                         "`redis_module!` calls through `src/redismodule.c`, calls it."),
    "GetClientId": ("internal", "`Context::acl_add_log_entry` only checks that the context has a "
                    "client; no wrapper returns the id."),
    "GetClientInfoById": ("internal", "`Context::block_client` only checks, before Redis 6.2, that "
                          "the client is in the client index; no wrapper returns client data."),
    "GetContextUser": ("internal", "`RuntimeContext::scoped_call_user` reads it to restore the "
                       "outer user later; no wrapper returns the context user."),
}

# Areas in document order: (area, [(name pattern, fit)]). The fit is the
# context view a wrapper would extend. The first pattern that matches the
# whole name wins.
AREAS = [
    ("Memory", [
        (r"(Try)?(Alloc|Calloc|Realloc)|Free|Strdup", "n/a"),
        (r"PoolAlloc", "command"),
        (r"MallocSize\w*|MallocUsableSize|GetUsedMemoryRatio", "context-free"),
    ]),
    ("Module lifecycle", [
        (r"GetApi|SetModuleAttribs|IsModuleNameBusy", "n/a"),
        (r"SetModuleOptions", "registration"),
        (r"GetModuleOptionsAll|GetContextFlagsAll|GetServerVersion|GetTypeMethodVersion",
         "context-free"),
    ]),
    ("Commands", [
        (r"CreateCommand|GetCommand|CreateSubcommand|SetCommandInfo|SetCommandACLCategories"
         r"|AddACLCategory", "registration"),
        (r"WrongArity|IsKeysPositionRequest|KeyAtPos(WithFlags)?|IsChannelsPositionRequest"
         r"|ChannelAtPosWithFlags|RedactClientCommandArgument|GetCurrentCommandName", "command"),
        (r"GetCommandKeys(WithFlags)?", "runtime"),
    ]),
    ("Replies", [(r"ReplyWith\w+|ReplySet\w+Length", "command")]),
    ("Calls", [(r"Call|CallReply\w+|FreeCallReply|CreateStringFromCallReply", "runtime")]),
    ("Strings", [
        (r"CreateString(From(LongLong|ULongLong|Double|String))?|CreateStringPrintf"
         r"|FreeString|StringPtrLen|StringTo(LongLong|ULongLong|Double)"
         r"|StringAppendBuffer|TrimStringAllocation|RetainString|StringCompare", "context-free"),
        (r"HoldString", "runtime"),
    ]),
    ("Keys and databases", [
        (r"GetSelectedDb|SelectDb|DbSize|RandomKey|KeyExists|SignalModifiedKey|OpenKey",
         "runtime"),
        (r"ResetDataset|GetOpenKeyModesAll", "context-free"),
        (r"CloseKey|KeyType|ValueLength|DeleteKey|UnlinkKey|(Get|Set)(Abs)?Expire"
         r"|(Get|Set)(LRU|LFU)|GetKeyNameFromModuleKey|GetDbIdFromModuleKey", "key"),
    ]),
    ("String values", [(r"StringSet|StringDMA|StringTruncate", "key")]),
    ("Lists", [(r"List(Push|Pop|Get|Set|Insert|Delete)", "key")]),
    ("Hashes", [(r"Hash(Set|Get|FieldMinExpire)", "key")]),
    ("Sorted sets", [(r"Zset\w+", "key")]),
    ("Streams", [
        (r"Stream\w+", "key"),
        (r"CreateStringFromStreamID|StringToStreamID", "context-free"),
    ]),
    ("Scanning", [(r"ScanCursor\w+|Scan", "runtime"), (r"ScanKey", "key")]),
    ("Native data types", [
        (r"CreateDataType", "registration"),
        (r"ModuleType(SetValue|ReplaceValue|GetType|GetValue)", "key"),
        (r"(Save|Load)(Unsigned|Signed|String|StringBuffer|Double|Float)|IsIOError"
         r"|EmitAOF|GetContextFromIO|GetKeyNameFromIO|GetDbIdFromIO|LogIOError",
         "callback (I/O)"),
        (r"LoadDataTypeFromString(Encver)?|SaveDataTypeToString", "runtime"),
        (r"Digest\w+|GetDbIdFromDigest|GetKeyNameFromDigest", "callback (digest)"),
        (r"Get(To)?(DbId|KeyName)FromOptCtx", "callback (key options)"),
    ]),
    ("RDB files", [(r"RdbStream\w+|RdbLoad|RdbSave", "runtime")]),
    ("Clients", [
        (r"GetClientId", "command"),
        (r"GetClient(UserName|Info|Name)ById|GetClientCertificate", "runtime"),
        (r"SetClientNameById", "context-free"),
    ]),
    ("Context state and replication", [
        (r"GetContextFlags|AvoidReplicaTraffic|Yield|Replicate", "runtime"),
        (r"ReplicateVerbatim|AutoMemory", "command"),
    ]),
    ("Pub/Sub", [(r"PublishMessage(Shard)?", "runtime")]),
    ("Logging and diagnostics", [(r"Log|LatencyAddSample", "context-free"), (r"_Assert", "n/a")]),
    ("Time and randomness", [
        (r"Milliseconds|(Monotonic|Cached)?Microseconds|GetRandom(Bytes|HexChars)",
         "context-free"),
    ]),
    ("Dictionaries", [(r"CreateDict|FreeDict|Dict\w+", "context-free")]),
    ("INFO", [
        (r"RegisterInfoFunc", "registration"),
        (r"InfoAdd\w+|InfoBeginDictField|InfoEndDictField", "callback (INFO)"),
        (r"GetServerInfo|FreeServerInfo|ServerInfoGetField\w*", "runtime"),
    ]),
    ("Server events", [(r"SubscribeToServerEvent", "event"), (r"IsSubEventSupported",
                                                              "context-free")]),
    ("Keyspace notifications", [
        (r"SubscribeToKeyspaceEvents|UnsubscribeFromKeyspaceEvents|AddPostNotificationJob",
         "event"),
        (r"NotifyKeyspaceEvent|GetNotifyKeyspaceEvents|GetKeyspaceNotificationFlagsAll",
         "runtime"),
    ]),
    ("Blocked clients", [
        (r"BlockClient(OnKeys(WithFlags)?)?|IsBlocked(Reply|Timeout)Request"
         r"|GetBlockedClientPrivateData|GetBlockedClientHandle|BlockedClientDisconnected"
         r"|GetBlockedClientReadyKey", "command"),
        (r"BlockClient(Get|Set)PrivateData|UnblockClient|AbortBlock"
         r"|BlockedClientMeasureTime(Start|End)|SetDisconnectCallback", "blocked client"),
        (r"SignalKeyAsReady", "runtime"),
    ]),
    ("Thread-safe contexts", [
        (r"GetThreadSafeContext|GetDetachedThreadSafeContext|FreeThreadSafeContext"
         r"|ThreadSafeContext(Lock|TryLock|Unlock)", "worker"),
    ]),
    ("Timers", [(r"CreateTimer|StopTimer|GetTimerInfo", "event")]),
    ("Event loop", [(r"EventLoop\w+", "context-free")]),
    ("Cluster", [
        (r"RegisterClusterMessageReceiver|SetClusterFlags", "registration"),
        (r"GetMyClusterID|GetClusterSize|FreeClusterNodesList|ClusterKeySlotC?"
         r"|ClusterCanonicalKeyNameInSlot|ClusterCanAccessKeysInSlot", "context-free"),
        (r"SendClusterMessage|GetClusterNodeInfo|GetClusterNodesList|Cluster\w+", "runtime"),
    ]),
    ("Shared APIs", [(r"ExportSharedAPI", "registration"), (r"GetSharedAPI", "runtime")]),
    ("Command filters", [
        (r"(Register|Unregister)CommandFilter", "registration"),
        (r"CommandFilter\w+", "callback (filter)"),
    ]),
    ("Fork", [(r"Fork|SendChildHeartbeat|ExitFromChild|KillForkChild", "context-free")]),
    ("ACL, users and authentication", [
        (r"RegisterAuthCallback", "registration"),
        (r"BlockClientOnAuth", "callback (auth)"),
        (r"CreateModuleUser|FreeModuleUser|GetUserUsername|SetModuleUserACL(String)?"
         r"|GetModuleUserACLString|GetModuleUserFromUserName|ACLCheck\w+", "context-free"),
        (r"SetContextUser|GetContextUser|GetCurrentUserName|GetInternalSecret", "runtime"),
        (r"ACLAddLogEntry(ByUserName)?|AuthenticateClientWith(ACL)?User", "command"),
        (r"DeauthenticateAndCloseClient", "runtime"),
    ]),
    ("Defragmentation", [
        (r"RegisterDefragFunc2?|RegisterDefragCallbacks", "registration"),
        (r"Defrag\w+|Get(DbId|KeyName)FromDefragCtx", "callback (defrag)"),
    ]),
    ("Module configuration", [
        (r"Register(Bool|Numeric|String|Enum)Config|LoadConfigs|LoadDefaultConfigs",
         "registration"),
    ]),
    ("Server configuration", [(r"Config(Iterator\w+|GetType|Get\w*|Set\w*)", "runtime")]),
    ("Key metadata", [
        (r"CreateKeyMetaClass|ReleaseKeyMetaClass", "registration"),
        (r"(Set|Get)KeyMeta", "key"),
    ]),
    ("Redis Enterprise extensions", [
        (r"SwapPrefetchKey|IsKeyInRam|Big\w+|SetOpenFilesReservation", "n/a"),
    ]),
]

# Covered another way, shown with the fit: name -> note.
EQUIVALENTS = {
    "ClusterKeySlot": "`cluster::key_slot` computes it",
    "ClusterKeySlotC": "`cluster::key_slot` computes it",
    "ClusterCanonicalKeyNameInSlot": "`cluster::key_for_slot` chooses a different name",
    "CreateStringFromLongLong": "format, then `RedisString::create`",
    "CreateStringFromULongLong": "format, then `RedisString::create`",
    "CreateStringFromDouble": "format, then `RedisString::create`",
    "CreateStringPrintf": "format, then `RedisString::create`",
    "CreateStringFromCallReply": "reply bytes, then `RedisString::create`",
    "ReplyWithCString": "string `RedisValue`s",
    "ReplyWithEmptyString": "string `RedisValue`s",
    "ReplyWithEmptyArray": "an empty `RedisValue::Array`",
    "ReplyWithErrorFormat": "format, then an error reply",
    "ReplyWithVerbatimString": "`RedisValue::VerbatimString`",
    "InfoAddFieldCString": "`InfoAddFieldString`, through the INFO builder",
    "GetCommandKeys": "`GetCommandKeysWithFlags`, through `get_command_keys`",
    "Microseconds": "`std::time::SystemTime`",
    "MonotonicMicroseconds": "`std::time::Instant`",
    "_Assert": "Rust panics",
}

# Requests for an API: upstream issues and pull requests, and this fork's
# issues. name -> references.
REQUESTS = {}
for _refs, _names in [
    (["upstream issue 235"], "ZsetAdd ZsetFirstInScoreRange ZsetRangeCurrentElement "
                             "ZsetRangeEndReached ZsetRangeNext ZsetRangeStop"),
    (["upstream PR 229"], "StreamIteratorDelete"),
    (["upstream issue 445", "upstream PR 446"], "RandomKey"),
    (["upstream issue 443", "upstream PR 444"], "SetLRU GetLRU SetLFU GetLFU"),
    (["upstream PR 437"], "UnregisterCommandFilter CommandFilterGetClientId"),
    (["upstream PRs 437, 168"], "RegisterCommandFilter CommandFilterArgsCount CommandFilterArgGet "
                                "CommandFilterArgInsert CommandFilterArgReplace "
                                "CommandFilterArgDelete"),
    (["upstream PR 469"], "GetKeyNameFromModuleKey"),
    (["upstream PR 391"], "GetExpire GetAbsExpire"),
    (["upstream PR 354"], "CreateModuleUser SetModuleUserACLString GetModuleUserACLString "
                          "AuthenticateClientWithUser"),
    (["upstream issue 381"], "RegisterAuthCallback"),
    (["upstream PRs 228, 169"], "CreateDict FreeDict"),
    (["upstream PR 228"], "DictReplace"),
    (["upstream PR 169"], "DictSet DictDel DictIteratorStart DictIteratorStop DictNext"),
    (["upstream issue 236"], "RegisterClusterMessageReceiver GetClusterNodeInfo "
                             "GetClusterNodesList FreeClusterNodesList GetMyClusterID "
                             "GetClusterSize SetClusterFlags"),
    (["upstream issues 438, 236"], "SendClusterMessage"),
    (["upstream issue 382"], "RdbLoad RdbSave"),
    (["upstream issue 74"], "EmitAOF"),
    (["upstream PR 100"], "HashGet"),
    (["upstream PR 482"], "Scan ScanKey"),
    (["#89"], "ThreadSafeContextTryLock GetRandomHexChars ClusterCanonicalKeyNameInSlot"),
    (["#55"], "ReplySetArrayLength ReplySetMapLength ReplySetSetLength"),
    (["#60"], "ConfigGet ConfigGetBool ConfigGetEnum ConfigGetNumeric ConfigGetType "
              "ConfigIteratorCreate ConfigIteratorNext ConfigIteratorRelease"),
]:
    for _name in _names.split():
        REQUESTS.setdefault(_name, []).extend(_refs)


# Rust source analysis.

TOKEN = re.compile(
    r"""
    (?P<ws>\s+)
  | (?P<line>//[^\n]*)
  | (?P<block>/\*)
  | (?P<raw>(?:br|cr|r)(?P<hashes>\#*)")
  | (?P<str>(?:b|c)?"(?:\\.|[^"\\])*")
  | (?P<char>b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])')
  | (?P<lifetime>'[A-Za-z_][A-Za-z0-9_]*)
  | (?P<ident>(?:r\#)?[A-Za-z_][A-Za-z0-9_]*)
  | (?P<num>[0-9][0-9A-Za-z_.]*)
  | (?P<punct>::|->|=>|.)
    """,
    re.S | re.X,
)


def tokenize(text):
    """(kind, text, line) tokens, without comments and string contents."""
    tokens, pos, line = [], 0, 1
    while pos < len(text):
        m = TOKEN.match(text, pos)
        end = m.end()
        if m.group("block"):
            depth = 1
            while depth and end < len(text):
                if text.startswith("/*", end):
                    depth, end = depth + 1, end + 2
                elif text.startswith("*/", end):
                    depth, end = depth - 1, end + 2
                else:
                    end += 1
        elif m.group("raw"):
            close = '"' + m.group("hashes")
            end = text.index(close, end) + len(close)
            tokens.append(("str", "", line))
        elif m.group("str"):
            tokens.append(("str", "", line))
        elif m.group("ident") or m.group("punct") or m.group("lifetime"):
            kind = "ident" if m.group("ident") else "punct"
            tokens.append((kind, m.group(0), line))
        line += text.count("\n", pos, end)
        pos = end
    return tokens


def matching(tokens, i):
    """Index of the token that closes the group opened at tokens[i]."""
    open_ = tokens[i][1]
    close = {"(": ")", "[": "]", "{": "}", "<": ">"}[open_]
    depth = 0
    for j in range(i, len(tokens)):
        if tokens[j][1] == open_:
            depth += 1
        elif tokens[j][1] == close:
            depth -= 1
            if not depth:
                return j
    return len(tokens) - 1


def header_end(tokens, i):
    """Index of the '{' or ';' that ends an item header starting at i."""
    depth = 0
    for j in range(i, len(tokens)):
        t = tokens[j][1]
        if t in ("(", "["):
            depth += 1
        elif t in (")", "]"):
            depth -= 1
        elif not depth and t in ("{", ";"):
            return j
    return len(tokens) - 1


def type_name(tokens):
    """The last path segment of a type, before its generic arguments."""
    name = None
    for kind, text, _ in tokens:
        if text in ("<", "where", "{"):
            break
        if kind == "ident" and text not in ("dyn", "mut", "const", "unsafe", "extern", "crate"):
            name = text
    return name


def attribute_flags(attrs):
    flags = set()
    for a in attrs:
        if a == "test" or (a.startswith("cfg(") and re.search(r"\btest\b", a)
                           and "not(test" not in a):
            flags.add("test")
        elif a == "doc(hidden)":
            flags.add("hidden")
        elif a == "macro_export":
            flags.add("export")
    return flags


class Item:
    """A function, method, macro_rules definition or static initializer."""

    def __init__(self, kind, name, module, line, **info):
        self.kind, self.name, self.module, self.line = kind, name, module, line
        self.vis = info.get("vis", "private")
        self.unsafe = info.get("unsafe", False)
        self.flags = set(info.get("flags", ()))
        self.impl_type = info.get("impl_type")
        self.trait = info.get("trait")
        self.trait_def = info.get("trait_def", False)
        self.method = info.get("method", False)
        self.container = info.get("container")
        self.file = info.get("file")
        self.apis = defaultdict(set)  # API name -> access modes
        self.refs = []  # (kind, name, qualifier)

    def label(self):
        if self.kind == "macro":
            return f"{self.name}!"
        if self.kind == "static":
            return self.name
        if self.container is not None:
            return f"{self.container.label()}::{self.name}"
        if self.impl_type and self.trait and not self.trait_def:
            return f"{self.impl_type}::{self.name} ({self.trait})"
        if self.impl_type:
            return f"{self.impl_type}::{self.name}"
        parts = self.module.split("::")[1:]
        return "::".join([*parts[-1:], self.name])


def file_module(path):
    parts = list(path.relative_to(SRC).with_suffix("").parts)
    if parts[-1] in ("lib", "mod"):
        parts.pop()
    return "::".join(["crate", *parts])


ITEM_START = {None, "}", ";", "{", "]"}
MODIFIERS = {"pub", "unsafe", "extern", "const", "async", "default", "crate", "super", "self",
             "in", "(", ")"}


def use_paths(tokens, base=()):
    """Expand a use tree into paths: a::{b, c::*} -> [a, b], [a, c, *]."""
    paths, current, i = [], list(base), 0
    while i < len(tokens):
        text = tokens[i][1]
        if text == "{":
            close = matching(tokens, i)
            paths += use_paths(tokens[i + 1:close], current)
            current, i = list(base), close + 1
            continue
        if text == ",":
            if current != list(base):
                paths.append(current)
            current = list(base)
        elif text == "as":
            i += 1
        elif text == "*" or tokens[i][0] == "ident":
            current = current + [text]
        i += 1
    if current != list(base):
        paths.append(current)
    return paths


class Crate:
    """Items, API references and visibility facts from the SDK's Rust source."""

    def __init__(self, apis):
        self.apis = apis
        self.items = []
        self.types = defaultdict(set)  # type or trait name -> visibilities
        self.modules = {"crate": "pub"}  # module path -> declared visibility
        self.reexports = []  # full paths
        self.names = {}  # file -> identifiers it uses
        self.imports = defaultdict(set)  # file -> names it imports
        self.globs = defaultdict(set)  # file -> modules it glob-imports
        self.hidden = set()  # names re-exported only for the macros

    def resolve(self, module, path):
        """A use path as a full path from the crate root."""
        parts = module.split("::")
        if path[0] == "crate":
            return path
        if path[0] in ("self", "super"):
            while path and path[0] in ("self", "super"):
                if path[0] == "super":
                    parts = parts[:-1]
                path = path[1:]
            return [*parts, *path]
        if f"{module}::{path[0]}" in self.modules:
            return [*parts, *path]
        return path

    def parse(self, path):
        tokens = tokenize(path.read_text())
        base = file_module(path)
        self.names[path] = {t[1] for t in tokens if t[0] == "ident"}
        stack, pending, attrs, api_list = [], {}, [], []
        test_file = False

        def find(kind):
            return next((f for f in reversed(stack) if f["kind"] == kind), None)

        def owner():
            return next((f["item"] for f in reversed(stack) if "item" in f), None)

        def module():
            return next((f["module"] for f in reversed(stack) if f["kind"] == "mod"), base)

        def inherited():
            flags = set()
            for f in stack:
                flags |= f.get("flags", set())
            return (flags | ({"test"} if test_file else set())) & {"test", "hidden"}

        i = 0
        while i < len(tokens):
            kind, text, line = tokens[i]
            prev = tokens[i - 1][1] if i else None
            nxt = tokens[i + 1][1] if i + 1 < len(tokens) else None
            nxt_ident = i + 1 < len(tokens) and tokens[i + 1][0] == "ident"

            if text == "#" and nxt in ("[", "!"):
                open_ = i + 1 if nxt == "[" else i + 2
                close = matching(tokens, open_)
                attr = "".join(t[1] for t in tokens[open_ + 1:close])
                if nxt == "[":
                    attrs.append(attr)
                elif "test" in attribute_flags([attr]):
                    test_file = True
                i = close + 1
                continue
            if text == "{":
                stack.append(pending.pop(i, {"kind": "block"}))
                attrs = []
            elif text == "}":
                if stack:
                    stack.pop()
                attrs = []
            elif text in (";", ","):
                attrs = []
            if kind != "ident":
                i += 1
                continue

            current = owner()
            flags = attribute_flags(attrs) | inherited()

            if text == "fn" and nxt_ident:
                j, unsafe, vis = i - 1, False, "private"
                while j >= 0 and (tokens[j][1] in MODIFIERS or tokens[j][0] == "str"):
                    if tokens[j][1] == "unsafe":
                        unsafe = True
                    elif tokens[j][1] == "pub":
                        vis = "restricted" if tokens[j + 1][1] == "(" else "pub"
                    j -= 1
                k = i + 2
                if tokens[k][1] == "<":
                    k = matching(tokens, k) + 1
                params = tokens[k:matching(tokens, k)] if tokens[k][1] == "(" else []
                start = header_end(tokens, i + 2)
                impl, trait = find("impl"), find("trait")
                top = current is None
                item = Item("fn", tokens[i + 1][1], module(), line, vis=vis, unsafe=unsafe,
                            flags=flags, container=current, file=path,
                            method=any(t[1] == "self" for t in params))
                if top and trait:
                    item.impl_type = item.trait = trait["name"]
                    item.trait_def, item.vis = True, trait["vis"]
                elif top and impl:
                    item.impl_type, item.trait = impl["type"], impl["trait"]
                for api in api_list:
                    item.apis[api].add("api!")
                api_list = []
                # Types named in the signature, such as a returned guard.
                item.refs += [("type", t[1], None) for t in tokens[i + 2:start] if t[0] == "ident"]
                if tokens[start][1] == "{":
                    pending[start] = {"kind": "fn", "item": item, "flags": flags}
                    self.items.append(item)
                attrs, i = [], start
                continue

            if text == "impl" and (prev in ITEM_START or prev == "unsafe") and nxt != "(":
                start = header_end(tokens, i + 1)
                k = matching(tokens, i + 1) + 1 if nxt == "<" else i + 1
                header = tokens[k:start]
                trait_name, self_type = None, header
                for n, t in enumerate(header):
                    if t[1] == "for" and (n + 1 == len(header) or header[n + 1][1] != "<"):
                        trait_name, self_type = type_name(header[:n]), header[n + 1:]
                        break
                pending[start] = {"kind": "impl", "type": type_name(self_type),
                                  "trait": trait_name, "flags": flags}
                attrs, i = [], start
                continue

            if text == "trait" and nxt_ident:
                start = header_end(tokens, i + 2)
                vis = "pub" if "pub" in (prev, tokens[i - 2][1] if i >= 2 else None) else "private"
                self.types[tokens[i + 1][1]].add(vis)
                if tokens[start][1] == "{":
                    pending[start] = {"kind": "trait", "name": tokens[i + 1][1], "vis": vis,
                                      "flags": flags}
                attrs, i = [], start
                continue

            if text == "mod" and nxt_ident and current is None:
                path_ = f"{module()}::{tokens[i + 1][1]}"
                vis = {"pub": "pub", ")": "restricted"}.get(prev, "private")
                self.modules[path_] = "test" if "test" in flags else vis
                if tokens[i + 2][1] == "{":
                    pending[i + 2] = {"kind": "mod", "module": path_, "flags": flags}
                attrs, i = [], i + 2
                continue

            if text in ("struct", "enum", "union", "type") and nxt_ident and current is None:
                self.types[tokens[i + 1][1]].add("pub" if prev == "pub" else "private")

            if text == "use" and nxt != "(":
                end = header_end(tokens, i + 1)
                if tokens[end][1] == "{":
                    end = matching(tokens, end) + 1
                for p in use_paths(tokens[i + 1:end]):
                    full = self.resolve(module(), p)
                    if full[-1] == "*":
                        self.globs[path].add("::".join(full[:-1]))
                    else:
                        self.imports[path].add(full[-1])
                    if prev == "pub" and current is None:
                        self.reexports.append(full)
                        if "hidden" in inherited():
                            self.hidden.add(full[-1])
                i = end
                continue

            if text == "macro_rules" and nxt == "!":
                item = Item("macro", tokens[i + 2][1], module(), line, flags=flags, file=path,
                            vis="pub" if "export" in flags else "private", container=current)
                self.items.append(item)
                pending[i + 3] = {"kind": "macro", "item": item, "flags": flags}
                attrs, i = [], i + 3
                continue

            # api!([RedisModule_X, ...], fn ...) names the APIs its function uses.
            if text == "api" and nxt == "!" and i + 3 < len(tokens) and tokens[i + 3][1] == "[":
                close = matching(tokens, i + 3)
                api_list = [t[1][len(PREFIX):] for t in tokens[i + 4:close]
                            if t[1].startswith(PREFIX)]
                i = close + 1
                continue

            if text.startswith(PREFIX) and text[len(PREFIX):] in self.apis:
                api = text[len(PREFIX):]
                before = [t[1] for t in tokens[max(0, i - 3):i]]
                mode = LOOKUPS.get(before[0], "direct") if before[1:] == ["!", "("] else "direct"
                target = current
                if target is None:
                    target = Item("static", f"{module()} line {line}", module(), line, flags=flags,
                                  file=path)
                    self.items.append(target)
                # In an api! function the name is a local binding of the pointer.
                if not (mode == "direct" and "api!" in target.apis.get(api, ())):
                    target.apis[api].add(mode)
            elif current is not None:
                if nxt == "!":
                    current.refs.append(("macro", text, None))
                elif prev == ".":
                    current.refs.append(("method", text, None))
                elif prev == "::":
                    qualifier = tokens[i - 2][1] if i >= 2 else None
                    if qualifier == "Self":
                        scope = next((f for f in reversed(stack)
                                      if f["kind"] in ("impl", "trait")), {})
                        qualifier = scope.get("type") or scope.get("name")
                    current.refs.append(("path", text, None if qualifier == ">" else qualifier))
                else:
                    current.refs += [("bare", text, None), ("type", text, None)]
            i += 1

    def parse_proc_macros(self, directory):
        """Paths generated by the public proc macros, as one safe entry."""
        item = Item("fn", "proc macros", "proc-macros", 0, vis="pub")
        for path in sorted(directory.rglob("*.rs")):
            tokens = tokenize(path.read_text())
            for i, (_, text, _) in enumerate(tokens):
                if text == "redis_module" and i + 2 < len(tokens) and tokens[i + 1][1] == "::":
                    j = i + 2
                    while j + 2 < len(tokens) and tokens[j + 1][1] == "::":
                        j += 2
                    item.refs.append(("path", tokens[j][1], tokens[j - 2][1]))
                    item.refs.append(("type", tokens[j - 2][1], None))
                    item.refs.append(("type", tokens[j][1], None))
        self.items.append(item)
        return item

    def public_modules(self):
        public = {m for m in self.modules if all(
            self.modules.get("::".join(m.split("::")[:n + 1]), "private") == "pub"
            for n in range(len(m.split("::"))))}
        globs, names = set(), set()
        for full in self.reexports:
            target = "::".join(full[:-1] if full[-1] == "*" else full)
            if full[-1] == "*":
                globs.add(target)
            elif target in self.modules:
                public.add(target)
            names.add(full[-1])
        return public | globs, names

    def graph(self, proc_entry):
        """Edges between items and each item's entry kind (safe, unsafe or None)."""
        by_name = defaultdict(list)
        trait_impls = defaultdict(list)
        for item in self.items:
            if item.kind == "fn":
                by_name[item.name].append(item)
                if item.trait and not item.trait_def and item.container is None:
                    trait_impls[item.impl_type].append(item)
        macros = defaultdict(list)
        for item in self.items:
            if item.kind == "macro":
                macros[item.name].append(item)

        edges = defaultdict(set)
        for item in self.items:
            if item.container is not None:
                edges[id(item.container)].add(id(item))
            for kind, name, qualifier in item.refs:
                if kind == "macro":
                    targets = macros.get(name, [])
                elif kind == "type":
                    targets = trait_impls.get(name, []) if name in self.types else []
                elif kind == "method":
                    # Without types, a method call can only be matched by name. Keep
                    # the methods of types the calling file names, or its own.
                    named = self.names.get(item.file, set())
                    targets = [c for c in by_name.get(name, []) if c.method and (
                        c.file == item.file or c.impl_type in named or c.trait in named)]
                elif kind == "bare":
                    # A free function in scope: nested in the caller, in its module,
                    # or imported by its file. Other bare names are locals.
                    imports, globs = self.imports[item.file], self.globs[item.file]
                    targets = [c for c in by_name.get(name, []) if c.impl_type is None and (
                        c.container is item or (c.container is None and (
                            c.module == item.module or name in imports or c.module in globs))
                        or (c.container is not None and c.container is item.container))]
                else:
                    candidates = by_name.get(name, [])
                    targets = candidates if qualifier is None else [
                        c for c in candidates if qualifier in (c.impl_type, c.trait)
                        or (c.impl_type is None and c.module.split("::")[-1] == qualifier)]
                    if not targets and qualifier and qualifier[0].islower():
                        targets = [c for c in candidates if c.impl_type is None]
                for target in targets:
                    if target is not item:
                        edges[id(item)].add(id(target))

        modules, names = self.public_modules()
        entries = {id(proc_entry): "safe"}
        for item in self.items:
            if "test" in item.flags or "hidden" in item.flags or item.container is not None:
                continue
            if item.kind == "macro":
                public = item.vis == "pub"
            elif item.kind != "fn":
                public = False
            elif item.trait_def:
                public = item.vis == "pub"
            elif item.impl_type in self.hidden:
                public = False
            elif item.trait:
                public = (self.types.get(item.impl_type, {"pub"}) != {"private"}
                          and self.types.get(item.trait, {"pub"}) != {"private"})
            elif item.impl_type:
                public = item.vis == "pub" and self.types.get(item.impl_type) != {"private"}
            else:
                public = item.vis == "pub" and (item.module in modules or item.name in names)
            if public:
                entries[id(item)] = "unsafe" if item.unsafe else "safe"
        return edges, entries


def reach(edges, starts):
    seen, queue = set(starts), deque(starts)
    while queue:
        for nxt in edges.get(queue.popleft(), ()):
            if nxt not in seen:
                seen.add(nxt)
                queue.append(nxt)
    return seen


def analyse(apis):
    crate = Crate(set(apis))
    for path in sorted(SRC.rglob("*.rs")):
        if BINDINGS not in path.parents:
            crate.parse(path)
    proc_entry = crate.parse_proc_macros(PROC_MACROS)
    edges, entries = crate.graph(proc_entry)
    items = {id(i): i for i in crate.items}
    safe = reach(edges, [i for i, k in entries.items() if k == "safe"])
    unsafe = reach(edges, [i for i, k in entries.items() if k == "unsafe"])
    reverse = defaultdict(set)
    for src, dsts in edges.items():
        for dst in dsts:
            reverse[dst].add(src)

    result = {}
    for api in apis:
        refs = [i for i in crate.items if api in i.apis and "test" not in i.flags]
        modes = set().union(*(i.apis[api] for i in refs)) if refs else set()
        ids = {id(i) for i in refs}
        if ids & safe:
            status = "safe"
        elif ids & unsafe:
            status = "unsafe"
        else:
            status = "internal" if refs else "none"
        via = []
        if status in ("safe", "unsafe"):
            # The nearest public entries, by reverse breadth-first search.
            seen, frontier = set(ids), list(ids)
            while frontier and not via:
                via = sorted({items[i].label() for i in frontier if entries.get(i) == status})
                nxt = []
                for i in frontier:
                    for p in reverse.get(i, ()):
                        if p not in seen:
                            seen.add(p)
                            nxt.append(p)
                frontier = nxt
        elif status == "internal":
            via = sorted({i.label() for i in refs})
        result[api] = {"status": status, "modes": modes, "via": via}
    return result


# Header, Redis sources and floors.

def header_apis():
    """The API pointers the header declares, and those it has commented out."""
    text = HEADER.read_text()
    declared = r"REDISMODULE_API\b[^;]*?\(\s*\*\s*" + PREFIX + r"(\w+)\s*\)"
    return (re.findall("^" + declared, text, re.M | re.S),
            re.findall(r"^//\s*" + declared, text, re.M))


def version_key(version):
    return tuple(int(p) for p in version.split("."))


def redis_versions(directory):
    """API name -> the first minor version, among module-<version>.c files,
    whose source defines it; the versions examined; and the APIs the newest
    source registers."""
    files = sorted(Path(directory).glob("module-*.c"),
                   key=lambda p: version_key(p.stem[len("module-"):]))
    if not files:
        sys.exit(f"no module-<version>.c files in {directory}")
    first = {}
    for path in files:
        text = path.read_text(errors="replace")
        version = path.stem[len("module-"):]
        # REGISTER_API(X) exports RM_X as RedisModule_X. RM_GetApi is not
        # registered: Redis passes it to modules in the context.
        names = set(re.findall(r"^\s*REGISTER_API\((\w+)\)", text, re.M))
        names |= set(re.findall(r"^[A-Za-z_][\w \t*]*?\bRM_(\w+)\s*\(", text, re.M))
        for name in names:
            first.setdefault(name, ".".join(version.split(".")[:2]))
    registered = set(re.findall(r"^\s*REGISTER_API\((\w+)\)", text, re.M))
    return first, [p.stem[len("module-"):] for p in files], registered


def floors():
    text = CARGO.read_text()
    found = re.findall(r"^min-redis-compatibility-version-(\d+)-(\d+)\s*=", text, re.M)
    return sorted({f"{a}.{b}" for a, b in found}, key=version_key)


def floor_for(introduced, floor_list):
    """The lowest floor at or above the version that introduced an API."""
    if not re.fullmatch(r"\d+\.\d+", introduced):
        return "none"
    return next((f for f in floor_list if version_key(f) >= version_key(introduced)), "none")


# Document.

def area_of(name):
    for area, rules in AREAS:
        for pattern, fit in rules:
            if re.fullmatch(pattern, name):
                return area, fit
    return None, None


SOURCES = re.compile(r"^Redis sources examined:\s+(.+?)\.\n\n", re.M | re.S)
UNDECLARED = re.compile(r"^The newest of them registers these APIs, which the header\s+does\s+not"
                        r"\s+declare:\s+(.+?)\.\n\n", re.M | re.S)
ROW = re.compile(r"^\| `(\w+)` \| ([^|]+?) \|", re.M)


def stored_versions():
    """The source facts recorded in the document: introduced versions, the
    sources examined and the newest source's undeclared APIs."""
    text = DOC.read_text() if DOC.exists() else ""
    if BEGIN not in text:
        return {}, [], []
    section = text.split(BEGIN, 1)[1].split(END, 1)[0]
    sources, undeclared = SOURCES.search(section), UNDECLARED.search(section)
    return (dict(ROW.findall(section)),
            re.split(r",\s+", sources.group(1)) if sources else [],
            re.findall(r"`(\w+)`", undeclared.group(1)) if undeclared else [])


def rows(apis, analysis, introduced, floor_list):
    out, unmatched = [], []
    order = {area: n for n, (area, _) in enumerate(AREAS)}
    for index, name in enumerate(apis):
        info = analysis[name]
        status, reason = info["status"], None
        if name in OVERRIDES:
            status, reason = OVERRIDES[name]
            if status == info["status"]:
                sys.exit(f"the override for {name} matches its automatic status; remove it")
        area, fit = area_of(name)
        if area is None:
            unmatched.append(name)
            continue
        if name in EQUIVALENTS:
            fit = f"{fit}; {EQUIVALENTS[name]}"
        version = introduced.get(name, "unknown")
        out.append({
            "name": name, "area": area, "introduced": version,
            "floor": floor_for(version, floor_list), "status": status,
            "modes": [m for m in MODES if m in info["modes"]],
            "via": info["via"], "fit": fit if status != "safe" else "",
            "requests": REQUESTS.get(name, []), "override": reason,
            "order": (order[area], index),
        })
    if unmatched:
        sys.exit(f"no area in AREAS for: {', '.join(unmatched)}")
    return sorted(out, key=lambda r: r["order"])


def cell(values, limit):
    values = list(values)
    shown = ", ".join(f"`{v}`" for v in values[:limit])
    return shown + (f" and {len(values) - limit} more" if len(values) > limit else "")


def wrap(text, indent=""):
    return textwrap.wrap(text, 80, subsequent_indent=indent, break_long_words=False,
                         break_on_hyphens=False)


def counts_row(label, table):
    return f"| {label} | {len(table)} | " + " | ".join(
        str(sum(r["status"] == s for r in table)) for s in STATUSES) + " |"


def render(table, floor_list, sources, undeclared, commented):
    counts = {s: sum(r["status"] == s for r in table) for s in STATUSES}
    lines = [
        BEGIN,
        "<!-- Generated by .github/scripts/api-coverage.py; edit the script instead. -->",
        "",
        *wrap(f"`src/include/redismodule.h` declares {len(table)} API pointers: "
              + ", ".join(f"{counts[s]} {s}" for s in STATUSES) + "."),
    ]
    if commented:
        lines += [
            "",
            *wrap(f"It also has {len(commented)} declarations commented out, which have no "
                  "binding and are not in the table: "
                  + ", ".join(f"`{n}`" for n in commented) + "."),
        ]
    lines += [
        "",
        *wrap(f"Redis sources examined: {', '.join(sources) if sources else 'none'}."),
    ]
    if undeclared:
        lines += [
            "",
            *wrap("The newest of them registers these APIs, which the header does not declare: "
                  + ", ".join(f"`{n}`" for n in undeclared) + "."),
        ]
    lines += [
        "",
        "### By compatibility floor",
        "",
        "APIs introduced in or before each floor's Redis version, by status. The",
        "last row counts APIs that no floor guarantees: newer than the highest",
        "floor, or not in the Redis OSS sources.",
        "",
        "| floor | APIs | " + " | ".join(STATUSES) + " |",
        "|---|---|" + "---|" * len(STATUSES),
    ]
    for floor in floor_list:
        lines.append(counts_row(floor, [r for r in table if r["floor"] != "none"
                                        and version_key(r["floor"]) <= version_key(floor)]))
    lines.append(counts_row("no floor", [r for r in table if r["floor"] == "none"]))

    early = [r for r in table if "required" in r["modes"] and r["floor"] != floor_list[0]]
    lines += [
        "",
        "### Required above the lowest floor",
        "",
        *wrap("`__required_api!` panics, naming the API, when the running Redis lacks it. "
              f"SDK code uses it for these APIs, which the {floor_list[0]} floor does not "
              "guarantee:"),
        "",
    ]
    lines += [f"- `{r['name']}` ({r['introduced']})" for r in early] or ["- None."]

    lines += ["", "### By area"]
    area = None
    for r in table:
        if r["area"] != area:
            area = r["area"]
            lines += ["", f"#### {area}", "",
                      "| API | introduced | floor | access | status | reached through | fit "
                      "| requests |",
                      "|---|---|---|---|---|---|---|---|"]
        status = r["status"] + (" (hand)" if r["override"] else "")
        lines.append(
            f"| `{r['name']}` | {r['introduced']} | {r['floor']} | {', '.join(r['modes'])} | "
            f"{status} | {cell(r['via'], 2)} | {r['fit']} | {', '.join(r['requests'])} |")

    hand = [r for r in table if r["override"]]
    if hand:
        lines += ["", "### Hand classifications", ""]
        for r in hand:
            lines += wrap(f"- `{r['name']}`, {r['status']}: {r['override']}", "  ")
    lines += ["", END]
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--redis-src", metavar="DIR",
                        help="directory of Redis module-<version>.c files; without it, the "
                             "introduced versions recorded in the document are reused")
    parser.add_argument("--check", action="store_true",
                        help="exit non-zero if the document is out of date; write nothing")
    parser.add_argument("--report", action="store_true",
                        help="print each API's automatic and final classification")
    args = parser.parse_args()

    apis, commented = header_apis()
    if len(apis) != len(set(apis)):
        sys.exit("the header declares an API pointer twice")
    for name in OVERRIDES:
        if name not in apis:
            sys.exit(f"override for {name}, which the header does not declare")
    floor_list = floors()
    if args.redis_src:
        first, sources, registered = redis_versions(args.redis_src)
        introduced = {name: first.get(name, "Enterprise" if name in ENTERPRISE else "not in OSS")
                      for name in apis}
        undeclared = sorted(registered - set(apis) - set(commented))
    else:
        introduced, sources, undeclared = stored_versions()
    missing = [n for n in apis if introduced.get(n, "unknown") == "unknown"]
    if missing:
        message = (f"{len(missing)} API(s) have no recorded introduced version, such as "
                   f"{missing[0]}; run with --redis-src")
        if args.check:
            sys.exit(message)
        print(message, file=sys.stderr)
    analysis = analyse(apis)
    table = rows(apis, analysis, introduced, floor_list)

    if args.report:
        for r in table:
            info = analysis[r["name"]]
            print("\t".join([r["name"], info["status"], r["status"], ",".join(r["modes"]),
                             "; ".join(r["via"]), r["introduced"]]))
        return

    text = DOC.read_text() if DOC.exists() else ""
    if BEGIN not in text or END not in text:
        sys.exit(f"{DOC.relative_to(ROOT)} lacks the {BEGIN} and {END} markers")
    head, rest = text.split(BEGIN, 1)
    updated = (head + render(table, floor_list, sources, undeclared, commented)
               + rest.split(END, 1)[1])
    if args.check:
        if updated != text:
            sys.exit(f"{DOC.relative_to(ROOT)} is out of date; run "
                     ".github/scripts/api-coverage.py")
        print(f"{DOC.relative_to(ROOT)} is up to date.")
        return
    if updated != text:
        DOC.write_text(updated)
        print(f"Updated {DOC.relative_to(ROOT)}.")
    else:
        print(f"{DOC.relative_to(ROOT)} is up to date.")


if __name__ == "__main__":
    main()
