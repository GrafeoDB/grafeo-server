(ns jepsen.grafeo.util
  "Docker control helpers and HTTP client for grafeo-server."
  (:require [clj-http.client :as http]
            [cheshire.core :as json]
            [clojure.java.shell :as shell]
            [clojure.tools.logging :refer [info warn]]))

;; ---------------------------------------------------------------------------
;; Node topology
;; ---------------------------------------------------------------------------

(def nodes
  "Logical node name -> HTTP base URL."
  {"primary"   "http://localhost:17474"
   "replica-1" "http://localhost:17475"
   "replica-2" "http://localhost:17476"})

(def containers
  "Logical node name -> Docker Compose service name."
  {"primary"   "primary"
   "replica-1" "replica-1"
   "replica-2" "replica-2"})

;; ---------------------------------------------------------------------------
;; Docker Compose helpers
;; ---------------------------------------------------------------------------

(defn compose!
  "Runs docker compose with the given args. Returns the shell result map.
   Throws on non-zero exit."
  [compose-file & args]
  (let [cmd (vec (concat ["docker" "compose" "-f" compose-file] args))
        result (apply shell/sh cmd)]
    (when (not= 0 (:exit result))
      (throw (ex-info (str "docker compose failed: " (:err result))
                      {:cmd cmd :result result})))
    result))

(defn start-cluster!
  "Starts the Docker replication cluster and waits for health checks."
  [compose-file]
  (info "Starting Docker cluster...")
  (compose! compose-file "up" "-d" "--wait")
  (info "Cluster ready"))

(defn stop-cluster!
  "Tears down the Docker cluster and removes volumes."
  [compose-file]
  (info "Stopping Docker cluster...")
  (compose! compose-file "down" "-v")
  (info "Cluster stopped"))

(defn pause-service!
  "Pauses a Docker Compose service (network partition simulation)."
  [compose-file service]
  (info "Pausing" service)
  (compose! compose-file "pause" service))

(defn unpause-service!
  "Unpauses a Docker Compose service."
  [compose-file service]
  (info "Unpausing" service)
  (compose! compose-file "unpause" service))

(defn kill-service!
  "Kills a Docker Compose service (crash simulation)."
  [compose-file service]
  (info "Killing" service)
  (compose! compose-file "kill" service))

(defn start-service!
  "Starts a killed Docker Compose service."
  [compose-file service]
  (info "Starting" service)
  (compose! compose-file "start" service))

;; ---------------------------------------------------------------------------
;; Health check
;; ---------------------------------------------------------------------------

(defn healthy?
  "Returns true if the node at `url` responds 200 to GET /health."
  [url]
  (try
    (let [resp (http/get (str url "/health")
                         {:socket-timeout 2000
                          :connection-timeout 2000
                          :throw-exceptions false})]
      (= 200 (:status resp)))
    (catch Exception _ false)))

(defn wait-for-healthy
  "Polls GET /health until 200 or timeout (ms). Returns true if healthy."
  [url timeout-ms]
  (let [start (System/currentTimeMillis)]
    (loop []
      (if (healthy? url)
        true
        (if (> (- (System/currentTimeMillis) start) timeout-ms)
          false
          (do (Thread/sleep 500) (recur)))))))

;; ---------------------------------------------------------------------------
;; HTTP client
;; ---------------------------------------------------------------------------

(defn gql-query
  "Executes a GQL query via POST /query. Returns parsed response body or throws."
  [url query-str]
  (let [resp (http/post (str url "/query")
                        {:content-type :json
                         :body (json/generate-string {:query query-str})
                         :as :json
                         :throw-exceptions false
                         :socket-timeout 5000
                         :connection-timeout 3000})]
    (if (= 200 (:status resp))
      (:body resp)
      (throw (ex-info (str "query failed (" (:status resp) ")")
                      {:status (:status resp) :body (:body resp)})))))

(defn node-count
  "Returns the number of nodes in the default database."
  [url]
  (let [resp (gql-query url "MATCH (n) RETURN count(n) AS cnt")]
    (get-in resp [:rows 0 0] 0)))

(defn read-all-keys
  "Returns a set of all JepsenNode key values on the given node."
  [url]
  (let [resp (gql-query url "MATCH (n:JepsenNode) RETURN n.key AS k ORDER BY k")]
    (->> (get resp :rows [])
         (map first)
         (filter some?)
         set)))
