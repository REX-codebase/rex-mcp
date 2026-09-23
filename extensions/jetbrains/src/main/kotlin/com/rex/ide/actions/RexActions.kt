package com.rex.ide.actions

import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.ui.Messages
import com.rex.ide.RexService

private fun service(): RexService =
    ApplicationManager.getApplication().getService(RexService::class.java)

class RexRunTaskAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val task = Messages.showInputDialog(
            e.project, "What should REX do?",
            "REX: Run Task", Messages.getQuestionIcon()
        )?.trim().orEmpty()
        if (task.isEmpty()) return
        try {
            service().startRun(task)
        } catch (ex: Exception) {
            service().notify("REX: cannot start run", ex.message ?: "unknown error", NotificationType.ERROR)
        }
    }
}

class RexCancelAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val s = service()
        val runId = s.activeRunId
        if (runId == null) {
            s.notify("REX", "No active run.", NotificationType.INFORMATION)
            return
        }
        try {
            s.daemon.cancel(runId)
            s.notify("REX", "Run cancelled.", NotificationType.INFORMATION)
        } catch (ex: Exception) {
            s.notify("REX: cancel failed", ex.message ?: "unknown error", NotificationType.ERROR)
        }
    }
}

class RexCheckpointAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val s = service()
        val runId = s.activeRunId
        if (runId == null) {
            s.notify("REX", "No active run.", NotificationType.INFORMATION)
            return
        }
        try {
            val res = s.daemon.checkpoint(runId)
            s.notify("REX", "Checkpoint ${res.get("checkpoint").asInt} saved.", NotificationType.INFORMATION)
        } catch (ex: Exception) {
            s.notify("REX: checkpoint failed", ex.message ?: "unknown error", NotificationType.ERROR)
        }
    }
}

class RexRewindAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val s = service()
        val runId = s.activeRunId
        if (runId == null) {
            s.notify("REX", "No active run.", NotificationType.INFORMATION)
            return
        }
        try {
            val list = s.daemon.checkpoints(runId).getAsJsonArray("checkpoints")
            if (list == null || list.size() == 0) {
                s.notify("REX", "No checkpoints yet.", NotificationType.INFORMATION)
                return
            }
            val options = (0 until list.size()).map { i ->
                val o = list.get(i).asJsonObject
                "Checkpoint ${o.get("checkpoint").asString} (${o.get("created_at")?.asString ?: "?"})"
            }.toTypedArray()
            val pick = Messages.showChooseDialog(
                e.project, "Rewind the run's working copy to…",
                "REX: Rewind", Messages.getWarningIcon(), options, options[0]
            )
            if (pick < 0) return
            val n = list.get(pick).asJsonObject.get("checkpoint").asString.toLong()
            val confirm = Messages.showYesNoDialog(
                e.project,
                "Rewind the run's working copy to checkpoint $n? The agent keeps its current step and plan (file-level rewind only).",
                "REX: Rewind", Messages.getWarningIcon()
            )
            if (confirm != Messages.YES) return
            s.daemon.rewind(runId, n)
            s.notify("REX", "Rewound to checkpoint $n.", NotificationType.INFORMATION)
        } catch (ex: Exception) {
            s.notify("REX: rewind failed", ex.message ?: "unknown error", NotificationType.ERROR)
        }
    }
}

class RexShowReceiptAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val s = service()
        val runId = s.activeRunId
        if (runId == null) {
            s.notify("REX", "No active run.", NotificationType.INFORMATION)
            return
        }
        try {
            val receipt = s.daemon.snapshot(runId)
            Messages.showMessageDialog(
                e.project,
                com.google.gson.GsonBuilder().setPrettyPrinting().create().toJson(receipt),
                "REX run receipt", Messages.getInformationIcon()
            )
        } catch (ex: Exception) {
            s.notify("REX: cannot fetch receipt", ex.message ?: "unknown error", NotificationType.ERROR)
        }
    }
}
