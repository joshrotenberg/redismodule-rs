(ns wasm-sync
  "Merge master into experiment/wasm-host in a new worktree and run wasm/ci/check.sh there; print the merge and check results. Commits the merge locally; pushes nothing.

  Follows \"Syncing the wasm experiment\" in CONTRIBUTING.md and the local
  reproduction in the branch's wasm/CI.md:

  1. Fetches the remote and adds a worktree at WORKTREE, detached at
     REMOTE/experiment/wasm-host. REMOTE defaults to the remote whose URL is
     joshrotenberg/redismodule-rs, which CONTRIBUTING.md calls `fork`.
  2. Merges REMOTE/master with --no-ff and the given message. On a conflict it
     stops with :status :failed, the conflicted files, and the worktree left
     mid-merge for resolving by hand.
  3. Copies the four snapshot lockfiles from wasm/ci/locks/ into place.
  4. Unless --skip-check, runs `cargo fetch --locked` at the root and for the
     three guest manifests, then wasm/ci/check.sh (which runs cargo offline),
     logging to target/wasm-check.log and target/fetch-*.log in the worktree.
     It reports check.sh's test counts and, comparing bytes, whether each
     lockfile still equals its snapshot. --skip-check is for
     documentation-only syncs, as earlier ones skipped check.sh; the push CI
     still runs it.

  It does not write the wasm/TRACKING.md entry or push; :next says what is
  left. Fetching and check.sh need the network, and the tests bind local
  ports, so in a sandbox that forbids those they fail."
  {:cj/args [{:name :worktree :type :string :positional true :required true
              :doc "Absolute path for the new worktree; must not exist"}
             {:name :message :type :string :required true :doc "Merge commit subject"}
             {:name :remote :type :string
              :doc "Remote that has master and experiment/wasm-host; default the fork's"}
             {:name :server :type :string
              :doc "redis-server for check.sh, put first on PATH; default redis-server on PATH"}
             {:name :skip-check :type :boolean :doc "Merge only, for documentation-only syncs"}]
   :cj/examples ["cj run wasm-sync /abs/path/wt-wasm --message \"chore: sync wasm host with the header sync\""
                 "cj run wasm-sync /abs/path/wt-wasm --message \"chore: sync wasm host with the docs\" --skip-check"]}
  (:require [babashka.fs :as fs]
            [clojure.string :as str]
            [project.steps :as steps]))

(def ^:private lockfiles
  {"wasm/ci/locks/workspace.lock" "Cargo.lock"
   "wasm/ci/locks/guest-sdk.lock" "wasm/guest-sdk/Cargo.lock"
   "wasm/ci/locks/hello.lock" "wasm/guests/hello/Cargo.lock"
   "wasm/ci/locks/bench.lock" "wasm/bench/guest/Cargo.lock"})

(def ^:private guest-manifests
  ["wasm/guest-sdk/Cargo.toml" "wasm/guests/hello/Cargo.toml" "wasm/bench/guest/Cargo.toml"])

(defn- git [cwd & args]
  (steps/run cwd (into ["git"] args)))

(defn- fork-remote
  "The remote whose URL names joshrotenberg/redismodule-rs, or origin."
  [repo]
  (let [names (str/split-lines (steps/text (git repo "remote")))]
    (or (first (filter #(str/includes? (steps/text (git repo "remote" "get-url" %))
                                       "joshrotenberg/redismodule-rs")
                       names))
        "origin")))

(defn- fail [result data]
  (merge {:status :failed :error (or (:error result) {:exit-code (steps/exit-code result)})
          :stderr (str/trim (get-in result [:stderr :text] ""))}
         data))

(defn- lockfiles-changed
  "The lockfiles whose bytes differ from their snapshots (they are untracked
  and ignored, so git status does not show them)."
  [worktree]
  (vec (for [[snapshot target] lockfiles
             :when (not (java.util.Arrays/equals
                         ^bytes (fs/read-all-bytes (fs/path worktree snapshot))
                         ^bytes (fs/read-all-bytes (fs/path worktree target))))]
         target)))

(defn- check [worktree server]
  (let [log-dir (str (fs/path worktree "target"))
        _ (fs/create-dirs log-dir)
        env (steps/env server)
        fetches (into [(steps/step worktree log-dir env
                                   {:name "fetch-workspace" :argv ["cargo" "fetch" "--locked"]})]
                      (map-indexed (fn [i manifest]
                                     (steps/step worktree log-dir env
                                                 {:name (str "fetch-guest-" (inc i))
                                                  :argv ["cargo" "fetch" "--locked"
                                                         "--manifest-path" manifest]}))
                                   guest-manifests))]
    (if-let [failed (first (filter #(= :fail (:status %)) fetches))]
      {:status :fail :failed-step failed}
      (let [result (steps/step worktree log-dir env
                               {:name "wasm-check" :argv ["bash" "wasm/ci/check.sh"]})
            changed (lockfiles-changed worktree)]
        (cond-> (assoc result :lockfiles-changed changed)
          (seq changed) (assoc :status :fail))))))

(defn -main [{:keys [worktree message remote server skip-check]}]
  (let [repo (System/getProperty "user.dir")]
    (when-not (fs/absolute? worktree)
      (throw (ex-info (str "the worktree path must be absolute: " worktree) {:kind :invalid-input})))
    (when (fs/exists? worktree)
      (throw (ex-info (str worktree " already exists") {:kind :invalid-input})))
    (let [remote (or remote (fork-remote repo))
          base-ref (str remote "/experiment/wasm-host")
          master-ref (str remote "/master")
          fetched (git repo "fetch" "--quiet" remote)]
      (if-not (steps/ok? fetched)
        (fail fetched {:step :fetch :remote remote})
        (let [added (git repo "worktree" "add" "--quiet" "--detach" worktree base-ref)]
          (if-not (steps/ok? added)
            (fail added {:step :worktree :remote remote})
            (let [base (steps/text (git worktree "rev-parse" "--short" "HEAD"))
                  master (steps/text (git worktree "rev-parse" "--short" master-ref))
                  merged (git worktree "merge" "--no-ff" master-ref "-m" message)]
              (if-not (steps/ok? merged)
                (let [conflicts (vec (remove str/blank?
                                             (str/split-lines
                                              (steps/text (git worktree "diff" "--name-only"
                                                               "--diff-filter=U")))))]
                  (fail merged {:step :merge :worktree worktree :base base :master master
                                :conflicts conflicts
                                :next [(str "resolve the conflicts in " worktree " and commit")
                                       "copy the snapshot lockfiles and run wasm/ci/check.sh there, as CONTRIBUTING.md describes"]}))
                (let [head (steps/text (git worktree "rev-parse" "--short" "HEAD"))
                      up-to-date? (= head base)
                      _ (doseq [[snapshot target] lockfiles]
                          (fs/copy (fs/path worktree snapshot) (fs/path worktree target)
                                   {:replace-existing true}))
                      result (cond-> {:worktree worktree :remote remote :base base :master master
                                      :merge (if up-to-date? :already-up-to-date head)}
                               (not up-to-date?)
                               (assoc :diff-stat (last (str/split-lines
                                                        (steps/text (git worktree "diff" "--stat" "HEAD^1" "HEAD"))))))
                      checked (when-not skip-check (check worktree server))
                      ok? (or skip-check (= :pass (:status checked)))]
                  (cond-> (assoc result :status (if ok? :completed :failed))
                    checked (assoc :check checked)
                    (and ok? up-to-date?)
                    (assoc :next [(str "nothing to sync; git -C " repo " worktree remove " worktree)])
                    (and ok? (not up-to-date?))
                    (assoc :next [(str "record the sync in " worktree "/wasm/TRACKING.md and commit it")
                                  (str "git -C " worktree " push " remote " HEAD:experiment/wasm-host")
                                  "confirm the experiment push CI"])))))))))))
