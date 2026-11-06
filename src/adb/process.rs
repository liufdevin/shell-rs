use std::{
    io,
    process::{Child, Command},
};

pub(super) struct CommandGroup {
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
    #[cfg(unix)]
    id: libc::pid_t,
}

impl CommandGroup {
    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW,
            };
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut child = command.spawn()?;
            if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0
            {
                let error = io::Error::last_os_error();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok((child, Self { job }))
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
            let child = command.spawn()?;
            let id = child.id() as libc::pid_t;
            Ok((child, Self { id }))
        }
    }

    pub(super) fn terminate(&self) {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(
                    self.job.as_raw_handle(),
                    1,
                );
            }
        }
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(-self.id, libc::SIGKILL);
            }
        }
    }

    pub(super) fn terminate_on_close(&self) -> io::Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::System::JobObjects::{
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JobObjectExtendedLimitInformation, SetInformationJobObject,
            };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if unsafe {
                SetInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn closing_a_mirror_job_terminates_its_process() {
        let program = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
        let mut command = Command::new(program);
        command.args(["/D", "/C", "for /L %G in (0,0,1) do @rem"]);
        let (mut child, group) = CommandGroup::spawn(&mut command).unwrap();
        group.terminate_on_close().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(child.try_wait().unwrap().is_none());
        drop(group);
        let started = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if started.elapsed() >= Duration::from_secs(3) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("closing the job left the mirror process running");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
