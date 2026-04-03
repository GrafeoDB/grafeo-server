(ns jepsen.grafeo.core
  "Jepsen test for grafeo-server async replication.

   Tests eventual consistency: writes go to the primary, reads come from
   any node including replicas. Nemeses inject network partitions (docker
   pause) and process crashes (docker kill). After healing, the checker
   verifies that every acknowledged write is visible on every node.

   Usage:
     cd jepsen
     lein run -- --time-limit 60 --concurrency 8
     lein run -- --time-limit 120 --concurrency 12 --nemesis true"
  (:require [jepsen.grafeo.util :as util]
            [clojure.pprint :refer [pprint]]
            [clojure.set :as set]
            [clojure.tools.logging :refer [info warn error]])
  (:import [java.util.concurrent ConcurrentLinkedQueue]
           [java.util.concurrent.atomic AtomicLong AtomicBoolean])
  (:gen-class))

;; ---------------------------------------------------------------------------
;; Operation recording
;; ---------------------------------------------------------------------------

(defrecord Op [type f value time process])

(defn now-nanos [] (System/nanoTime))

;; ---------------------------------------------------------------------------
;; Workers
;; ---------------------------------------------------------------------------

(defn writer-worker
  "Writes unique keys to the primary, recording operations to `history`."
  [^ConcurrentLinkedQueue history ^AtomicLong counter ^AtomicBoolean running
   process-id primary-url]
  (while (.get running)
    (let [v (.incrementAndGet counter)
          k (str "k" v)
          t0 (now-nanos)]
      (.add history (->Op :invoke :write {:key k :value v} t0 process-id))
      (try
        (util/gql-query primary-url
                        (str "INSERT (:JepsenNode {key: '" k "', val: " v "})"))
        (.add history (->Op :ok :write {:key k :value v} (now-nanos) process-id))
        (catch Exception e
          (.add history (->Op :fail :write {:key k :value v :error (.getMessage e)}
                              (now-nanos) process-id))))
      (Thread/sleep (+ 20 (rand-int 30))))))

(defn reader-worker
  "Reads all JepsenNode keys from a random node, recording to `history`."
  [^ConcurrentLinkedQueue history ^AtomicBoolean running process-id]
  (while (.get running)
    (let [urls (vec (vals util/nodes))
          url (rand-nth urls)
          t0 (now-nanos)]
      (.add history (->Op :invoke :read nil t0 process-id))
      (try
        (let [keys (util/read-all-keys url)]
          (.add history (->Op :ok :read {:found keys :node url} (now-nanos) process-id)))
        (catch Exception _
          (.add history (->Op :fail :read nil (now-nanos) process-id))))
      (Thread/sleep (+ 50 (rand-int 100))))))

;; ---------------------------------------------------------------------------
;; Nemesis worker
;; ---------------------------------------------------------------------------

(def replica-services ["replica-1" "replica-2"])

(defn nemesis-worker
  "Periodically partitions and heals replicas."
  [^ConcurrentLinkedQueue history ^AtomicBoolean running compose-file]
  (while (.get running)
    (let [svc (rand-nth replica-services)]
      ;; Partition
      (.add history (->Op :info :partition {:service svc} (now-nanos) :nemesis))
      (try (util/pause-service! compose-file svc) (catch Exception _))
      (Thread/sleep (+ 3000 (rand-int 5000)))
      ;; Heal
      (.add history (->Op :info :heal {:service svc} (now-nanos) :nemesis))
      (try (util/unpause-service! compose-file svc) (catch Exception _))
      (Thread/sleep (+ 2000 (rand-int 3000))))))

;; ---------------------------------------------------------------------------
;; Checker
;; ---------------------------------------------------------------------------

(defn check-convergence
  "Verifies eventual consistency: every acknowledged write must be visible
   on every node after replication catches up."
  []
  (let [;; All writes that the primary acknowledged
        primary-url (get util/nodes "primary")]
    (fn [history]
      (let [ack-writes (->> history
                            (filter #(and (= :ok (:type %))
                                         (= :write (:f %))))
                            (map (comp :key :value))
                            set)
            ;; Final reads from every node
            final-reads (into {}
                              (for [[node-name url] util/nodes]
                                (try
                                  [node-name (util/read-all-keys url)]
                                  (catch Exception _
                                    [node-name :unreachable]))))
            ;; Check convergence per node
            missing (into {}
                          (for [[node-name found] final-reads
                                :when (set? found)
                                :let [diff (set/difference ack-writes found)]
                                :when (seq diff)]
                            [node-name diff]))
            ;; Nodes that are unreachable
            unreachable (->> final-reads
                             (filter (fn [[_ v]] (= v :unreachable)))
                             (map first)
                             vec)
            valid? (and (empty? missing) (empty? unreachable))]
        {:valid?           valid?
         :ack-write-count  (count ack-writes)
         :per-node-found   (into {} (for [[n v] final-reads]
                                      [n (if (set? v) (count v) v)]))
         :missing          (when (seq missing) missing)
         :unreachable      (when (seq unreachable) unreachable)}))))

;; ---------------------------------------------------------------------------
;; Test runner
;; ---------------------------------------------------------------------------

(defn run-test!
  "Runs the replication test. Returns the operation history."
  [{:keys [compose-file time-limit concurrency nemesis?]
    :or   {time-limit 30 concurrency 4 nemesis? true}}]
  (let [history   (ConcurrentLinkedQueue.)
        running   (AtomicBoolean. true)
        counter   (AtomicLong. 0)
        n-writers (max 1 (int (Math/ceil (* concurrency 0.6))))
        n-readers (max 1 (- concurrency n-writers))

        primary-url (get util/nodes "primary")

        threads
        (vec
         (concat
          ;; Writer threads
          (for [i (range n-writers)]
            (Thread. #(writer-worker history counter running i primary-url)
                     (str "writer-" i)))
          ;; Reader threads
          (for [i (range n-readers)]
            (Thread. #(reader-worker history running (+ n-writers i))
                     (str "reader-" i)))
          ;; Nemesis thread
          (when nemesis?
            [(Thread. #(nemesis-worker history running compose-file)
                      "nemesis")])))]

    ;; Start all threads
    (doseq [t threads] (.start t))

    ;; Run for time-limit seconds
    (info "Running for" time-limit "seconds,"
          n-writers "writers," n-readers "readers,"
          (if nemesis? "nemesis ON" "nemesis OFF"))
    (Thread/sleep (* time-limit 1000))

    ;; Stop workers
    (.set running false)
    (doseq [t threads] (.join t 10000))

    ;; Ensure all replicas are unpaused for final reads
    (doseq [svc replica-services]
      (try (util/unpause-service! compose-file svc) (catch Exception _)))

    ;; Wait for replication to catch up
    (info "Waiting 15s for replication convergence...")
    (Thread/sleep 15000)

    (vec history)))

;; ---------------------------------------------------------------------------
;; CLI
;; ---------------------------------------------------------------------------

(defn parse-opts
  "Parses CLI args into an options map."
  [args]
  (loop [args (seq args) opts {}]
    (if (nil? args)
      opts
      (let [[k v & rest] args]
        (recur rest
               (case k
                 "--time-limit"  (assoc opts :time-limit (Integer/parseInt v))
                 "--concurrency" (assoc opts :concurrency (Integer/parseInt v))
                 "--nemesis"     (assoc opts :nemesis? (Boolean/parseBoolean v))
                 "--no-cluster"  (assoc opts :skip-cluster? true)
                 "--compose-file" (assoc opts :compose-file v)
                 (do (warn "unknown arg:" k) opts)))))))

(def default-compose-file "../tests/docker-compose.replication.yml")

(defn -main [& args]
  (let [opts (merge {:compose-file default-compose-file}
                    (parse-opts args))
        skip-cluster? (:skip-cluster? opts)]
    (try
      (when-not skip-cluster?
        (util/start-cluster! (:compose-file opts)))

      (let [history (run-test! opts)
            op-count (count history)
            checker-fn (check-convergence)
            result (checker-fn history)]
        (info "Test complete." op-count "operations recorded")
        (println "\n=== Results ===")
        (pprint result)
        (println)
        (if (:valid? result)
          (do (println "PASS") (System/exit 0))
          (do (println "FAIL") (System/exit 1))))

      (catch Exception e
        (error e "Test failed with exception")
        (System/exit 2))

      (finally
        (when-not skip-cluster?
          (try (util/stop-cluster! (:compose-file opts))
               (catch Exception _)))))))
