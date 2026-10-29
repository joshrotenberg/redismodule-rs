(ns project.steps
  "Running commands for this repository's recipes: each with its full output
  logged to a file and a short result returned as data."
  (:require [babashka.fs :as fs]
            [clajent.proc :as proc]
            [clojure.string :as str]))

(def ^:private output-limit (* 16 1024 1024))

(defn env
  "Environment overlay for cargo and the integration tests. PATH puts
  `~/.cargo/bin` (when it exists) first, so the toolchain rust-toolchain.toml
  pins wins, then the directory of `server` when it is a path, so the
  processes cargo and check.sh start find that redis-server.
  TEST_REDIS_SERVER_BIN names `server`, made absolute when it is a path; a bare
  name is left to PATH."
  [server]
  (let [cargo-bin (str (fs/path (fs/home) ".cargo" "bin"))
        server (if (and server (str/includes? server "/")) (str (fs/absolutize server)) server)
        dirs (cond-> []
               (fs/directory? cargo-bin) (conj cargo-bin)
               (and server (str/includes? server "/")) (conj (str (fs/parent server))))]
    (cond-> {}
      (seq dirs) (assoc "PATH" (str/join ":" (conj dirs (System/getenv "PATH"))))
      server (assoc "TEST_REDIS_SERVER_BIN" server))))

(defn- resolve-program
  "`program` as found on the overlay's PATH. clajent.proc starts argv[0] from
  the recipe's own PATH, so a bare name is resolved here first."
  [program env]
  (if (or (str/includes? program "/") (nil? (get env "PATH")))
    program
    (or (some (fn [dir]
                (let [candidate (fs/path dir program)]
                  (when (and (fs/regular-file? candidate) (fs/executable? candidate))
                    (str candidate))))
              (str/split (get env "PATH") #":"))
        program)))

(defn exit-code [result]
  (get-in result [:process :exit-code]))

(defn ok? [result]
  (= 0 (exit-code result)))

(defn text
  "Trimmed stdout of a finished command."
  [result]
  (str/trim (get-in result [:stdout :text] "")))

(defn run
  "Runs `argv` in `cwd` with `env` over the inherited environment and returns
  the proc/run! result."
  ([cwd argv] (run cwd argv {}))
  ([cwd argv env]
   (let [[program & args] argv]
     (proc/run! {:argv (into [(resolve-program program env)] args) :cwd cwd :env env
                 :stdout-limit-bytes output-limit :stderr-limit-bytes output-limit}))))

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
  "Runs one named step in `cwd`, writes its stdout then its stderr to
  `log-dir/NAME.log`, and returns its name, :pass or :fail, duration, log path,
  test counts when it ran tests, :truncated? when a stream outgrew the
  capture, and on failure the exit code and the last lines of each stream
  (cargo test writes failures to stdout and its summary to stderr)."
  [cwd log-dir env {:keys [name argv] :as spec}]
  (let [result (run cwd argv (merge env (:env spec)))
        stdout (get-in result [:stdout :text] "")
        stderr (get-in result [:stderr :text] "")
        log (str (fs/path log-dir (str name ".log")))
        counts (test-counts stdout)
        truncated? (boolean (or (get-in result [:stdout :truncated?])
                                (get-in result [:stderr :truncated?])))]
    (spit log (str stdout stderr))
    (cond-> {:name name
             :status (if (ok? result) :pass :fail)
             :seconds (quot (or (:duration-ms result) 0) 1000)
             :log log}
      counts (assoc :tests counts)
      truncated? (assoc :truncated? true)
      (not (ok? result)) (assoc :exit-code (exit-code result)
                                :error (:error result)
                                :stdout-tail (tail stdout 25)
                                :stderr-tail (tail stderr 15)))))
