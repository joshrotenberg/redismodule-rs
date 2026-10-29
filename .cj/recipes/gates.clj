(ns gates
  "Run CONTRIBUTING.md's validation list in a checkout; print each step's result and the test counts. Writes only target/.

  Runs every step even after one fails, in this order: fmt, Clippy at the
  default and 7.2 floors, examples, workspace tests, the allocator fallback
  test, strict rustdoc, then examples, tests and strict rustdoc at the 7.2
  floor, and the doctests with RUSTC_BOOTSTRAP=1 so compile_fail error codes
  are checked. On experiment/wasm-host (a checkout with wasm/Cargo.toml) the
  7.2-floor workspace steps exclude redis-module-wasm, as CONTRIBUTING.md
  says; wasm/ci/check.sh covers the wasm host.

  Each step's full output goes to target/gates/NAME.log in the checkout. The
  result lists every step with :pass or :fail, its duration, its log, test
  counts for steps that ran tests, and for a failed step its exit code and
  last 30 lines. :status is :failed when any step failed, so cj run exits 1.

  The integration tests start redis-server from PATH, or --server. They bind
  local ports, so in a sandbox that forbids that the test steps fail."
  {:cj/args [{:name :dir :type :string :doc "Checkout to validate; default the current directory"}
             {:name :server :type :string :doc "redis-server for the integration tests; default redis-server on PATH"}
             {:name :only :type :string :doc "Comma-separated step names to run, such as fmt,clippy,clippy-7.2"}]
   :cj/examples ["cj run gates"
                 "cj run gates --only fmt,clippy,clippy-7.2"
                 "cj run gates --dir /abs/path/to/worktree --server /opt/homebrew/bin/redis-server"]}
  (:require [babashka.fs :as fs]
            [clojure.string :as str]
            [project.steps :as steps]))

(def ^:private floor-7-2
  ["--no-default-features" "--features" "min-redis-compatibility-version-7-2"])

(defn- validation-steps
  "CONTRIBUTING.md's validation list, repeated at the 7.2 floor."
  [wasm?]
  (let [exclude (when wasm? ["--exclude" "redis-module-wasm"])
        threads {"RUST_TEST_THREADS" "4"}
        strict-doc {"RUSTDOCFLAGS" "-D warnings"}]
    [{:name "fmt" :argv ["cargo" "fmt" "--all" "--" "--check"]}
     {:name "clippy" :argv ["cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"]}
     {:name "clippy-7.2"
      :argv (concat ["cargo" "clippy" "--workspace" "--all-targets"] exclude floor-7-2
                    ["--" "-D" "warnings"])}
     {:name "examples" :argv ["cargo" "build" "--examples"]}
     {:name "test" :argv ["cargo" "test" "--workspace" "--no-fail-fast"] :env threads}
     {:name "alloc-fallback"
      :argv ["cargo" "test" "--features" "system-allocator-fallback"
             "--test" "system_allocator_fallback"]}
     {:name "doc"
      :argv ["cargo" "doc" "--workspace" "--no-deps" "--features" "all-features-but-xor"]
      :env strict-doc}
     {:name "examples-7.2" :argv (concat ["cargo" "build" "--examples"] floor-7-2)}
     {:name "test-7.2"
      :argv (concat ["cargo" "test" "--workspace" "--no-fail-fast"] exclude floor-7-2)
      :env threads}
     {:name "doc-7.2"
      :argv (concat ["cargo" "doc" "--workspace" "--no-deps"] exclude floor-7-2)
      :env strict-doc}
     {:name "doctest-bootstrap" :argv ["cargo" "test" "--doc"] :env {"RUSTC_BOOTSTRAP" "1"}}]))

(defn -main [{:keys [dir server only]}]
  (let [dir (str (fs/canonicalize (or dir (System/getProperty "user.dir"))))
        _ (when-not (fs/exists? (fs/path dir "Cargo.toml"))
            (throw (ex-info (str dir " has no Cargo.toml") {:kind :invalid-input})))
        wasm? (fs/exists? (fs/path dir "wasm" "Cargo.toml"))
        all-steps (validation-steps wasm?)
        selected (when only (set (map str/trim (str/split only #","))))
        unknown (remove (set (map :name all-steps)) selected)
        _ (when (seq unknown)
            (throw (ex-info (str "unknown steps: " (str/join ", " unknown)
                                 "; known: " (str/join ", " (map :name all-steps)))
                            {:kind :invalid-input})))
        log-dir (str (fs/path dir "target" "gates"))
        _ (fs/create-dirs log-dir)
        env (steps/env server)
        head (steps/text (steps/run dir ["git" "rev-parse" "--short" "HEAD"]))
        results (mapv #(steps/step dir log-dir env %)
                      (cond->> all-steps selected (filter (comp selected :name))))
        failed (filterv #(= :fail (:status %)) results)]
    {:dir dir
     :head head
     :wasm-branch? wasm?
     :status (if (seq failed) :failed :completed)
     :failed (mapv :name failed)
     :steps results}))
