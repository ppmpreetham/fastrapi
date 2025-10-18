import http from "k6/http";
import { check, sleep } from "k6";
import { Trend } from "k6/metrics";

const latency = new Trend("latency");

export const options = {
  stages: [
    { duration: "5s", target: 20 },   // ramp up from 0 to 20 VUs
    { duration: "15s", target: 50 },  // hold load
    { duration: "5s", target: 0 },    // ramp down
  ],
  thresholds: {
    http_req_failed: ["rate<0.01"],   // <1% requests can fail
    http_req_duration: ["p(95)<300"], // 95% responses under 300ms
  },
};

const BASE_URL = "http://127.0.0.1:8080/echo";

export default function () {
  const payload = JSON.stringify({
    message: `Hello from VU ${__VU}, iteration ${__ITER}`,
    timestamp: Date.now(),
  });

  const headers = { "Content-Type": "application/json" };

  const res = http.post(BASE_URL, payload, { headers });
  latency.add(res.timings.duration);

  check(res, {
    "status is 200": (r) => r.status === 200,
    "echo matches": (r) => r.json().received?.message?.startsWith("Hello"),
  });

}
