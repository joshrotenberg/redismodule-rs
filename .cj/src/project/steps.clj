(ns project.steps
  "Running commands for this repository's recipes: each with its full output
  logged to a file and a short result returned as data."
  (:require [babashka.fs :as fs]
            [clajent.proc :as proc]
            [clojure.string :as str]))

(def ^:private output-limit (* 16 1024 1024))

(defn env
  "Environment overlay for cargo and the integration tests: `~/.cargo/bin`
  first on PATH when it exists, so the toolchain rust-toolchain.toml pins wins
  over another rustc, and TEST_REDIS_SERVER_BIN when `server` is given."
  [server]
  (let [cargo-bin (str (fs/path (fs/home) ".cargo" "bin"))
        path (System/getenv "PATH")]
    (cond-> {}
      (fs/directory? cargo-bin) (assoc "PATH" (str cargo-bin ":" path))
      server (assoc "TEST_REDIS_SERVER_BIN" server))))

(defn exit-code [result]
  (get-in result [:process :exit-code]))

(defn ok? [result]
  (= 0 (exit-code result)))

(defn text
  "Trimmed stdout of a finished command."
  [result]
  (str/trim (get-in result [:stdout :text] "")))

(defn run
  "Runs `argv` in `cwd` and returns the proc/run! result."
  ([cwd argv] (run cwd argv {}))
  ([cwd argv env]
   (proc/run! {:argv (vec argv) :cwd cwd :env env
               :stdout-limit-bytes output-limit :stderr-limit-bytes output-limit})))

(defn test-counts
  "Sums the `test result:` lines cargo test prints; nil when there are none."
  [output]
  (when-let [lines (seq (re-seq #"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored"
                                output))]
    (reduce (fn [counts [_ passed failed ignored]]
              (-> counts
                  (update :passed + (parse-long passed))
                  (update :failed + (parse-long failed))
                  (update :ignored + (parse-long ignored))))
            {:passed 0 :failed 0 :ignored 0}
            lines)))

(defn- tail [output n]
  (let [lines (str/split-lines output)]
    (str/join "\n" (drop (max 0 (- (count lines) n)) lines))))

(defn step
  "Runs one named step in `cwd`, writes its stdout and stderr to
  `log-dir/NAME.log`, and returns its name, :pass or :fail, duration, log path,
  test counts when it ran tests, and on failure the exit code and the last
  lines of output."
  [cwd log-dir env {:keys [name argv] :as spec}]
  (let [result (run cwd argv (merge env (:env spec)))
        output (str (get-in result [:stdout :text] "") (get-in result [:stderr :text] ""))
        log (str (fs/path log-dir (str name ".log")))
        counts (test-counts output)]
    (spit log output)
    (cond-> {:name name
             :status (if (ok? result) :pass :fail)
             :seconds (quot (or (:duration-ms result) 0) 1000)
             :log log}
      counts (assoc :tests counts)
      (not (ok? result)) (assoc :exit-code (exit-code result)
                                :error (:error result)
                                :tail (tail output 30)))))
