package com.rex.ide

import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Application-level service: owns the daemon handle, the active run, and
 * the 2s poll that surfaces plan/tool approvals as notifications.
 */
@Service
class RexService {
    val daemon = RexDaemon()
    @Volatile var activeRunId: String? = null
    private val scheduler = Executors.newSingleThreadScheduledExecutor()
    private var pollTask: ScheduledFuture<*>? = null
    // Ask once per pending decision, not once per poll tick.
    private var lastPromptKey: String? = null

    @Synchronized
    fun startRun(task: String) {
        val runId = daemon.startRun(task)
        activeRunId = runId
        lastPromptKey = null
        startPolling()
        notify("REX run started", "Run ${runId.take(18)}… — approvals will appear here.", NotificationType.INFORMATION)
    }

    @Synchronized
    fun stopPolling() {
        pollTask?.cancel(false)
        pollTask = null
    }

    private fun startPolling() {
        stopPolling()
        pollTask = scheduler.scheduleAtFixedRate({
            try {
                pollOnce()
            } catch (_: Exception) {
                // Poll failures are transient; the next tick retries.
            }
        }, 0, 2, TimeUnit.SECONDS)
    }

    private fun pollOnce() {
        val runId = activeRunId ?: run { stopPolling(); return }
        val snap = try {
            daemon.snapshot(runId)
        } catch (e: Exception) {
            return
        }
        if (!snap.get("live").asBoolean) {
            val status = snap.get("status")?.asString ?: "finished"
            activeRunId = null
            stopPolling()
            notify(
                "REX run $status", "Run ${runId.take(18)}… finished: $status.",
                if (status == "Completed") NotificationType.INFORMATION else NotificationType.WARNING
            )
            return
        }
        if (snap.get("awaiting_plan")?.asBoolean == true) {
            val key = "plan:$runId"
            if (lastPromptKey != key) {
                lastPromptKey = key
                val plan = snap.getAsJsonArray("plan")?.joinToString("\n") { item ->
                    val o = item.asJsonObject
                    "- " + (o.get("title")?.asString ?: o.get("description")?.asString ?: o.toString())
                } ?: "(no plan details)"
                askApproval(
                    "REX wants to execute this plan", plan, key,
                    onDecision = { approved -> daemon.approve(runId, approved); lastPromptKey = null }
                )
            }
            return
        }
        val pa = snap.getAsJsonObject("pending_approval")
        if (pa != null) {
            val key = "tool:$runId:${pa.get("call_id")?.asString}"
            if (lastPromptKey != key) {
                lastPromptKey = key
                val text = "${pa.get("tool")?.asString}\n${pa.get("summary")?.asString}\n(${pa.get("policy_reason")?.asString})"
                askApproval(
                    "REX wants to run a tool", text, key,
                    onDecision = { approved -> daemon.approve(runId, approved); lastPromptKey = null }
                )
            }
        }
    }

    private fun askApproval(title: String, text: String, key: String, onDecision: (Boolean) -> Unit) {
        val group = NotificationGroupManager.getInstance().getNotificationGroup("rex.approvals")
        val n = group.createNotification(title, text, NotificationType.WARNING)
        n.addAction(object : NotificationAction("Approve") {
            override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                runDecision(onDecision, true, notification)
            }
        })
        n.addAction(object : NotificationAction("Deny") {
            override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                runDecision(onDecision, false, notification)
            }
        })
        n.notify(null)
    }

    private fun runDecision(onDecision: (Boolean) -> Unit, approved: Boolean, n: Notification) {
        try {
            onDecision(approved)
        } catch (e: Exception) {
            notify("REX approval failed", e.message ?: "unknown error", NotificationType.ERROR)
        } finally {
            n.expire()
        }
    }

    fun notify(title: String, text: String, type: NotificationType) {
        ApplicationManager.getApplication().invokeLater {
            NotificationGroupManager.getInstance()
                .getNotificationGroup("rex.approvals")
                .createNotification(title, text, type)
                .notify(null)
        }
    }

    fun shutdown() {
        stopPolling()
        scheduler.shutdownNow()
        daemon.stop()
    }
}
