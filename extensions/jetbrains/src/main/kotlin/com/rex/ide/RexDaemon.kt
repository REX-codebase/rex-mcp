package com.rex.ide

import com.google.gson.Gson
import com.google.gson.JsonObject
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.time.Duration

/**
 * Thin client over the local `rex serve` daemon (loopback only).
 *
 * The daemon is spawned as `rex serve --port 0`; its port is read from the
 * first stdout line (`{"port": N}`). All calls are plain HTTP/1.1 JSON.
 */
class RexDaemon(private val rexBin: String = "rex") {
    private val gson = Gson()
    private val http = HttpClient.newBuilder()
        .connectTimeout(Duration.ofSeconds(5))
        .build()

    private var process: Process? = null
    var port: Int? = null
        private set

    @Synchronized
    fun ensureStarted(): Int {
        port?.let { return it }
        val pb = ProcessBuilder(rexBin, "serve", "--port", "0")
            .redirectErrorStream(false)
        val p = pb.start()
        process = p
        val reader = BufferedReader(InputStreamReader(p.inputStream))
        val deadline = System.currentTimeMillis() + 15_000
        while (System.currentTimeMillis() < deadline) {
            val line = reader.readLine() ?: break
            try {
                val obj = gson.fromJson(line, JsonObject::class.java)
                if (obj.has("port")) {
                    val prt = obj.get("port").asInt
                    port = prt
                    return prt
                }
            } catch (_: Exception) {
                // Not the port line; keep waiting.
            }
        }
        p.destroy()
        process = null
        throw IllegalStateException("timed out waiting for `$rexBin serve` to announce its port")
    }

    fun stop() {
        process?.destroy()
        process = null
        port = null
    }

    private fun call(method: String, path: String, body: Any? = null): JsonObject {
        val p = ensureStarted()
        val builder = HttpRequest.newBuilder(URI("http://127.0.0.1:$p$path"))
            .timeout(Duration.ofSeconds(30))
            .header("Content-Type", "application/json")
        val payload = if (body == null) "" else gson.toJson(body)
        builder.method(method, HttpRequest.BodyPublishers.ofString(payload))
        val resp = http.send(builder.build(), HttpResponse.BodyHandlers.ofString())
        val parsed = if (resp.body().isBlank()) JsonObject()
        else gson.fromJson(resp.body(), JsonObject::class.java)
        if (resp.statusCode() >= 400) {
            val msg = if (parsed.has("error")) parsed.get("error").asString
            else "HTTP ${resp.statusCode()}"
            throw IllegalStateException("rex daemon: $msg")
        }
        return parsed
    }

    fun startRun(task: String, provider: String? = null): String {
        val body = mutableMapOf<String, Any>("task" to task)
        if (!provider.isNullOrBlank()) body["provider"] = provider
        // 202 Accepted carries the run id.
        return call("POST", "/v1/runs", body).get("run_id").asString
    }

    fun snapshot(runId: String): JsonObject = call("GET", "/v1/runs/$runId")

    fun approve(runId: String, approved: Boolean) {
        call("POST", "/v1/runs/$runId/approve", mapOf("approve" to approved))
    }

    fun cancel(runId: String) {
        call("POST", "/v1/runs/$runId/cancel")
    }

    fun checkpoint(runId: String): JsonObject =
        call("POST", "/v1/runs/$runId/checkpoint")

    fun checkpoints(runId: String): JsonObject =
        call("GET", "/v1/runs/$runId/checkpoints")

    fun rewind(runId: String, n: Long) {
        call("POST", "/v1/runs/$runId/rewind", mapOf("checkpoint" to n))
    }
}
