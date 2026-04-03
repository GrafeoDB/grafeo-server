(defproject jepsen.grafeo "0.1.0-SNAPSHOT"
  :description "Jepsen test for Grafeo graph database replication"
  :license {:name "Apache-2.0"}
  :dependencies [[org.clojure/clojure "1.11.1"]
                 [jepsen "0.3.6"]
                 [clj-http "3.12.3"]
                 [cheshire "5.12.0"]]
  :main jepsen.grafeo.core
  :profiles {:uberjar {:aot :all}})
