(ns wasm-sync
  "Merge master into experiment/wasm-host in a new worktree and run wasm/ci/check.sh there; print the merge and check results. Commits the merge locally; pushes nothing.

  Follows \"Syncing the wasm experiment\" in CONTRIBUTING.md:

  1. Fetches REMOTE and adds a worktree at WORKTREE, detached at
     REMOTE/experiment/wasm-host.
  2. Merges REMOTE/master with --no-ff and the given message. On a conflict it
     stops with :status :failed, the conflicted files, and the worktree left
     mid-merge for resolving by hand.
  3. Copies the four snapshot lockfiles from wasm/ci/locks/ into place.
  4. Unless --skip-check (for documentation-only syncs), runs
     wasm/ci/check.sh, logging to target/wasm-check.log in the worktree, and
     reports its test counts and whether the lockfiles stayed byte-identical.

  It does not write the wasm/TRACKING.md entry or push; :next says what is
  left. check.sh needs the network and binds local ports, so in a sandbox
  that forbids those it fails."
  {:cj/args [{:name :worktree :type :string :positional true :required true
              :doc "Absolute path for the new worktree; must not exist"}
             {:name :message :type :string :required true :doc "Merge commit subject"}
             {:name :remote :type :string :default "origin" :doc "Remote that has master and experiment/wasm-host"}
             {:name :server :type :string :doc "redis-server for check.sh; default redis-server on PATH"}
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

(defn- git [cwd & args]
  (steps/run cwd (into ["git"] args)))

(defn- fail [result data]
  (merge {:status :failed :error (or (:error result) {:exit-code (steps/exit-code result)})
          :stderr (str/trim (get-in result [:stderr :text] ""))}
         data))

(defn- check [worktree server]
  (let [log-dir (str (fs/path worktree "target"))
        _ (fs/create-dirs log-dir)
        result (steps/step worktree log-dir (steps/env server)
                           {:name "wasm-check" :argv ["bash" "wasm/ci/check.sh"]})
        changed (steps/text (git worktree "status" "--porcelain"))]
    (assoc result :lockfiles-unchanged? (str/blank? changed))))

(defn -main [{:keys [worktree message remote server skip-check]}]
  (let [repo (System/getProperty "user.dir")
        base-ref (str remote "/experiment/wasm-host")
        master-ref (str remote "/master")]
    (when-not (fs/absolute? worktree)
      (throw (ex-info (str "the worktree path must be absolute: " worktree) {:kind :invalid-input})))
    (when (fs/exists? worktree)
      (throw (ex-info (str worktree " already exists") {:kind :invalid-input})))
    (let [fetched (git repo "fetch" "--quiet" remote)]
      (if-not (steps/ok? fetched)
        (fail fetched {:step :fetch})
        (let [added (git repo "worktree" "add" "--quiet" "--detach" worktree base-ref)]
          (if-not (steps/ok? added)
            (fail added {:step :worktree})
            (let [base (steps/text (git worktree "rev-parse" "--short" "HEAD"))
                  master (steps/text (git worktree "rev-parse" "--short" master-ref))
                  merged (git worktree "merge" "--no-ff" master-ref "-m" message)]
              (if-not (steps/ok? merged)
                (fail merged {:step :merge :worktree worktree :base base :master master
                              :conflicts (str/split-lines
                                          (steps/text (git worktree "diff" "--name-only" "--diff-filter=U")))})
                (let [head (steps/text (git worktree "rev-parse" "--short" "HEAD"))
                      up-to-date? (= head base)
                      _ (doseq [[snapshot target] lockfiles]
                          (fs/copy (fs/path worktree snapshot) (fs/path worktree target)
                                   {:replace-existing true}))
                      result (cond-> {:worktree worktree :base base :master master
                                      :merge (if up-to-date? :already-up-to-date head)}
                               (not up-to-date?)
                               (assoc :diff-stat (last (str/split-lines
                                                        (steps/text (git worktree "diff" "--stat" "HEAD^1" "HEAD"))))))
                      checked (when-not skip-check (check worktree server))
                      ok? (or skip-check (and (= :pass (:status checked)) (:lockfiles-unchanged? checked)))]
                  (cond-> (assoc result :status (if ok? :completed :failed))
                    checked (assoc :check checked)
                    (and ok? up-to-date?)
                    (assoc :next [(str "nothing to sync; git -C " repo " worktree remove " worktree)])
                    (and ok? (not up-to-date?))
                    (assoc :next [(str "record the sync in " worktree "/wasm/TRACKING.md and commit it")
                                  (str "git -C " worktree " push " remote " HEAD:experiment/wasm-host")
                                  "confirm the experiment push CI"])))))))))))
