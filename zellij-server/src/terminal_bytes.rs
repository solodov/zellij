use crate::{
    os_input_output::AsyncReader, pty_writer::PtyWriteInstruction, screen::ScreenInstruction,
    thread_bus::ThreadSenders,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::{task, time::timeout};
use zellij_utils::{
    errors::{get_current_ctx, prelude::*, ContextType},
    logging::debug_to_file,
};

const WRITE_AFTER_INITIAL_OUTPUT_QUIET_FOR: Duration = Duration::from_millis(100);

pub(crate) struct TerminalBytes {
    terminal_id: u32,
    senders: ThreadSenders,
    async_reader: Box<dyn AsyncReader>,
    debug: bool,
    activity_flag: Arc<AtomicBool>,
    write_after_initial_output_settles: Option<PtyWriteInstruction>,
}

impl TerminalBytes {
    /// Create a PTY reader and optionally queue input until initial terminal output settles.
    pub fn new(
        terminal_id: u32,
        async_reader: Box<dyn AsyncReader>,
        senders: ThreadSenders,
        debug: bool,
        activity_flag: Arc<AtomicBool>,
        write_after_initial_output_settles: Option<PtyWriteInstruction>,
    ) -> Self {
        TerminalBytes {
            terminal_id,
            senders,
            debug,
            async_reader,
            activity_flag,
            write_after_initial_output_settles,
        }
    }
    pub async fn listen(&mut self) -> Result<()> {
        // This function reads bytes from the pty and then sends them as
        // ScreenInstruction::PtyBytes to screen to be parsed there
        // We also send a separate instruction to Screen to render as ScreenInstruction::Render
        //
        // We endeavour to send a Render instruction to screen immediately after having send bytes
        // to parse - this is so that the rendering is quick and smooth. However, this can cause
        // latency if the screen is backed up. For this reason, if we detect a peak in the time it
        // takes to send the render instruction, we assume the screen thread is backed up and so
        // only send a render instruction sparingly, giving screen time to process bytes and render
        // while still allowing the user to see an indication that things are happening (the
        // sparing render instructions)
        let err_context = || "failed to listen for bytes from PTY".to_string();

        let mut err_ctx = get_current_ctx();
        err_ctx.add_call(ContextType::AsyncTask);
        let mut buf = [0u8; 65536];
        let mut waiting_for_initial_output_to_settle = false;
        loop {
            let read_result = if waiting_for_initial_output_to_settle
                && self.write_after_initial_output_settles.is_some()
            {
                match timeout(
                    WRITE_AFTER_INITIAL_OUTPUT_QUIET_FOR,
                    self.async_reader.read(&mut buf),
                )
                .await
                {
                    Ok(read_result) => read_result,
                    Err(_) => {
                        if let Some(write_instruction) =
                            self.write_after_initial_output_settles.take()
                        {
                            self.async_send_to_pty_writer(write_instruction)
                                .await
                                .with_context(err_context)?;
                        }
                        waiting_for_initial_output_to_settle = false;
                        continue;
                    },
                }
            } else {
                self.async_reader.read(&mut buf).await
            };

            match read_result {
                Ok(0) => break, // EOF
                Err(err) => {
                    log::error!("{}", err);
                    break;
                },
                Ok(n_bytes) => {
                    self.activity_flag.store(true, Ordering::Relaxed);
                    let bytes = &buf[..n_bytes];
                    if self.debug {
                        let _ = debug_to_file(bytes, self.terminal_id as i32);
                    }
                    self.async_send_to_screen(ScreenInstruction::PtyBytes(
                        self.terminal_id,
                        bytes.to_vec(),
                    ))
                    .await
                    .with_context(err_context)?;
                    // Shell startup can produce several output bursts before the prompt. Wait
                    // until those bursts go quiet before injecting restored command input.
                    waiting_for_initial_output_to_settle =
                        self.write_after_initial_output_settles.is_some();
                },
            }
        }

        // Ignore any errors that happen here.
        // We only leave the loop above when the pane exits. This can happen in a lot of ways, but
        // the most problematic is when quitting zellij with `Ctrl+q`. That is because the channel
        // for `Screen` will have exited already, so this send *will* fail. This isn't a problem
        // per-se because the application terminates anyway, but it will print a lengthy error
        // message into the log for every pane that was still active when we quit the application.
        // This:
        //
        // 1. Makes the log rather pointless, because even when the application exits "normally",
        //    there will be errors inside and
        // 2. Leaves the impression we have a bug in the code and can't terminate properly
        //
        // FIXME: Ideally we detect whether the application is being quit and only ignore the error
        // in that particular case?
        let _ = self.async_send_to_screen(ScreenInstruction::Render).await;

        Ok(())
    }
    async fn async_send_to_screen(
        &self,
        screen_instruction: ScreenInstruction,
    ) -> Result<Duration> {
        // returns the time it blocked the thread for
        let sent_at = Instant::now();
        let senders = self.senders.clone();
        task::spawn_blocking(move || senders.send_to_screen(screen_instruction))
            .await
            .context("failed to async-send to screen")?
            .context("failed to block on sending message to screen")?;
        Ok(sent_at.elapsed())
    }

    async fn async_send_to_pty_writer(
        &self,
        write_instruction: PtyWriteInstruction,
    ) -> Result<Duration> {
        let sent_at = Instant::now();
        let senders = self.senders.clone();
        task::spawn_blocking(move || senders.send_to_pty_writer(write_instruction))
            .await
            .context("failed to async-send to pty writer")?
            .context("failed to block on sending message to pty writer")?;
        Ok(sent_at.elapsed())
    }
}
