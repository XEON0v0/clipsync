import java.nio.file.Files
import java.util.concurrent.ConcurrentLinkedQueue
import uniffi.clipboard_core.CoreCallbacks
import uniffi.clipboard_core.CoreHandle
import uniffi.clipboard_core.CoreStatus
import uniffi.clipboard_core.FfiClipContent
import uniffi.clipboard_core.FfiClipItem
import uniffi.clipboard_core.FfiHistoryKind
import uniffi.clipboard_core.FfiHistorySource
import uniffi.clipboard_core.MailboxDisposition
import uniffi.clipboard_core.PairingSnapshot

// Kotlin/JNA host closed loop (T10): two CoreHandles pair through the real
// relay over the generated UniFFI/JNA bindings; a live text clip round-trips
// through the typed CoreCallbacks surface.
//
// Args: <relay-ws-url>; JVM needs -Djna.library.path=<dir of libclipboard_core>

class HostCallbacks(
    private val mailboxDisposition: MailboxDisposition = MailboxDisposition.APPLIED,
) : CoreCallbacks {
    val clips = ConcurrentLinkedQueue<FfiClipItem>()
    val mailboxClips = ConcurrentLinkedQueue<FfiClipItem>()

    override fun onClip(item: FfiClipItem) {
        clips.add(item)
    }

    override fun onMailboxClip(item: FfiClipItem): MailboxDisposition {
        mailboxClips.add(item)
        return mailboxDisposition
    }

    override fun onStatus(status: CoreStatus) {}

    fun receivedTexts(): List<String> =
        clips.mapNotNull { item -> (item.content as? FfiClipContent.Text)?.text }
}

private fun waitSas(handle: CoreHandle): String {
    val deadline = System.currentTimeMillis() + 10_000
    while (System.currentTimeMillis() < deadline) {
        val snapshot = handle.pairPoll()
        if (snapshot is PairingSnapshot.SasReady) return snapshot.sas
        Thread.sleep(25)
    }
    error("FAIL: peer SAS did not arrive")
}

private fun waitTexts(callbacks: HostCallbacks, count: Int): List<String> {
    val deadline = System.currentTimeMillis() + 10_000
    while (System.currentTimeMillis() < deadline) {
        val texts = callbacks.receivedTexts()
        if (texts.size >= count) return texts
        Thread.sleep(25)
    }
    error("FAIL: live clip callback did not arrive")
}

private fun waitMailbox(callbacks: HostCallbacks): FfiClipItem {
    val deadline = System.currentTimeMillis() + 10_000
    while (System.currentTimeMillis() < deadline) {
        callbacks.mailboxClips.peek()?.let { return it }
        Thread.sleep(25)
    }
    error("FAIL: deferred mailbox callback did not arrive")
}

fun main(args: Array<String>) {
    require(args.size == 1) { "usage: ClosedLoop <relay-ws-url>" }
    val relay = args[0]
    val dirA = Files.createTempDirectory("clipsync-kt-a").toString()
    val dirB = Files.createTempDirectory("clipsync-kt-b").toString()

    val callbacksA = HostCallbacks()
    val callbacksB = HostCallbacks()
    val handleA = CoreHandle(dirA, callbacksA)
    val handleB = CoreHandle(dirB, callbacksB)

    val qr = handleA.pairBegin(relay)
    val sasB = handleB.pairClaim(qr)
    val sasA = waitSas(handleA)
    check(sasA == sasB) { "FAIL: SAS mismatch between hosts" }
    handleA.pairConfirm(sasA)
    handleB.pairConfirm(sasB)

    val seq = handleA.sendText("kotlin closed loop")
    check(seq == 1uL) { "FAIL: unexpected first sequence number $seq" }
    val texts = waitTexts(callbacksB, 1)
    check(texts == listOf("kotlin closed loop")) { "FAIL: unexpected callback payload $texts" }

    handleB.shutdown()
    Thread.sleep(200)
    val mailboxSeq = handleA.sendText("kotlin deferred mailbox")
    check(mailboxSeq == 2uL) { "FAIL: unexpected mailbox sequence number $mailboxSeq" }
    Thread.sleep(200)

    val resumedCallbacks = HostCallbacks(MailboxDisposition.DEFERRED)
    val resumedB = CoreHandle(dirB, resumedCallbacks)
    check(resumedB.pairLoad()) { "FAIL: receiver pairing record was not durable" }
    resumedB.start()
    val mailboxItem = waitMailbox(resumedCallbacks)
    val historyItem = resumedB.history().singleOrNull { it.id == mailboxItem.id }
        ?: error("FAIL: mailbox callback item missing from durable history")
    check(historyItem.source == FfiHistorySource.REMOTE_DEFERRED) {
        "FAIL: deferred mailbox history source was ${historyItem.source}"
    }
    val historyKind = historyItem.kind
    check(historyKind is FfiHistoryKind.Text && historyKind.content == "kotlin deferred mailbox") {
        "FAIL: deferred mailbox history content was $historyKind"
    }

    handleA.shutdown()
    resumedB.shutdown()
    println("PASS: Kotlin host closed loop delivered live clip over UniFFI/JNA FFI")
    println("PASS: Kotlin host stored deferred mailbox as durable RemoteDeferred history")
}
