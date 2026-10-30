use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::TaskScheduler::*;
use windows::Win32::System::Variant::*;
use windows::core::*;

use crate::store::ScheduleConfig;

pub struct SchedulerManager {
    service: ITaskService,
    pub task_name: String,
}

impl SchedulerManager {
    pub fn new(storage_dir: &Path, is_portable: bool) -> Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_ALL)?;

            service.Connect(
                &VARIANT::default(),
                &VARIANT::default(),
                &VARIANT::default(),
                &VARIANT::default(),
            )?;

            let task_name = if is_portable {
                let mut hasher = DefaultHasher::new();
                storage_dir.hash(&mut hasher);
                format!("AutoUpdate_{:08x}", hasher.finish() as u32)
            } else {
                "AutoUpdate".to_string()
            };

            Ok(Self { service, task_name })
        }
    }

    /// Sync schedule according to ScheduleConfig and current executable path (Spec 6.3)
    pub fn sync_schedule(&self, cfg: &ScheduleConfig, exe_path: &Path) -> Result<()> {
        if !cfg.enabled || (!cfg.on_logon && cfg.interval_minutes == 0) {
            // Disabled or no triggers -> delete task
            let _ = self.delete_task();
            return Ok(());
        }

        self.register_or_update_task(cfg, exe_path)
    }

    pub fn delete_task(&self) -> Result<()> {
        unsafe {
            let folder = self.get_or_create_folder(false)?;
            let bstr_name = BSTR::from(self.task_name.as_str());
            folder.DeleteTask(&bstr_name, 0)?;
            Ok(())
        }
    }

    fn get_or_create_folder(&self, create: bool) -> Result<ITaskFolder> {
        unsafe {
            let root_bstr = BSTR::from(r"\");
            let root_folder = self.service.GetFolder(&root_bstr)?;

            let sub_bstr = BSTR::from("Kabaper");
            match root_folder.GetFolder(&sub_bstr) {
                Ok(f) => Ok(f),
                Err(e) => {
                    if create {
                        root_folder.CreateFolder(&sub_bstr, &VARIANT::default())
                    } else {
                        Err(e)
                    }
                }
            }
        }
    }

    pub fn register_or_update_task(&self, cfg: &ScheduleConfig, exe_path: &Path) -> Result<()> {
        unsafe {
            let folder = self.get_or_create_folder(true)?;
            let task_def = self.service.NewTask(0)?;

            // 1. Registration info
            let reg_info = task_def.RegistrationInfo()?;
            let author_bstr = BSTR::from("Kabaper");
            let _ = reg_info.SetAuthor(&author_bstr);
            let desc_bstr = BSTR::from("Kabaper automatic wallpaper update");
            let _ = reg_info.SetDescription(&desc_bstr);

            // 2. Principal (Spec 6.3: TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_LUA)
            let principal = task_def.Principal()?;
            let _ = principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN);
            let _ = principal.SetRunLevel(TASK_RUNLEVEL_LUA);

            // 3. Settings (Spec 6.3)
            let settings = task_def.Settings()?;
            let _ = settings.SetDisallowStartIfOnBatteries(VARIANT_BOOL::from(false));
            let _ = settings.SetStopIfGoingOnBatteries(VARIANT_BOOL::from(false));
            let _ = settings.SetWakeToRun(VARIANT_BOOL::from(false));
            let _ = settings.SetStartWhenAvailable(VARIANT_BOOL::from(true));
            let _ = settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW);
            let _ = settings.SetEnabled(VARIANT_BOOL::from(true));

            // 4. Triggers
            let triggers = task_def.Triggers()?;

            if cfg.on_logon {
                let logon_trig = triggers.Create(TASK_TRIGGER_LOGON)?;
                let _ = logon_trig.SetEnabled(VARIANT_BOOL::from(true));
            }

            if cfg.interval_minutes > 0 {
                let time_trig = triggers.Create(TASK_TRIGGER_TIME)?;
                let _ = time_trig.SetEnabled(VARIANT_BOOL::from(true));

                // Start now
                let now_str = chrono_like_now_iso();
                let start_bstr = BSTR::from(now_str.as_str());
                let _ = time_trig.SetStartBoundary(&start_bstr);

                let repetition = time_trig.Repetition()?;
                let duration_bstr = BSTR::from(format!("PT{}M", cfg.interval_minutes));
                let _ = repetition.SetInterval(&duration_bstr);
            }

            // 5. Actions: "<exe_path>" --random
            let actions = task_def.Actions()?;
            let action = actions.Create(TASK_ACTION_EXEC)?;
            let exec_action: IExecAction = action.cast()?;

            let exe_bstr = BSTR::from(exe_path.to_string_lossy().as_ref());
            let args_bstr = BSTR::from("--random");
            exec_action.SetPath(&exe_bstr)?;
            exec_action.SetArguments(&args_bstr)?;

            // 6. Register
            let task_name_bstr = BSTR::from(self.task_name.as_str());
            let empty_var = VARIANT::default();
            folder.RegisterTaskDefinition(
                &task_name_bstr,
                &task_def,
                TASK_CREATE_OR_UPDATE.0,
                &empty_var,
                &empty_var,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty_var,
            )?;

            Ok(())
        }
    }

    /// Check if registered task exe path needs updating (Spec 6.3)
    pub fn check_and_repair_exe_path(&self, current_exe: &Path) -> Result<()> {
        unsafe {
            let Ok(folder) = self.get_or_create_folder(false) else {
                return Ok(());
            };
            let bstr_name = BSTR::from(self.task_name.as_str());
            let Ok(task) = folder.GetTask(&bstr_name) else {
                return Ok(());
            };

            let task_def = task.Definition()?;
            let actions = task_def.Actions()?;
            let mut count = 0i32;
            actions.Count(&mut count)?;
            if count > 0 {
                let action = actions.get_Item(1)?;
                if let Ok(exec_action) = action.cast::<IExecAction>() {
                    let mut path_bstr = BSTR::default();
                    exec_action.Path(&mut path_bstr)?;
                    let registered_path = PathBuf::from(path_bstr.to_string());

                    if registered_path != current_exe {
                        let new_exe_bstr = BSTR::from(current_exe.to_string_lossy().as_ref());
                        exec_action.SetPath(&new_exe_bstr)?;
                        let empty_var = VARIANT::default();
                        folder.RegisterTaskDefinition(
                            &bstr_name,
                            &task_def,
                            TASK_CREATE_OR_UPDATE.0,
                            &empty_var,
                            &empty_var,
                            TASK_LOGON_INTERACTIVE_TOKEN,
                            &empty_var,
                        )?;
                    }
                }
            }
            Ok(())
        }
    }
}

fn chrono_like_now_iso() -> String {
    unsafe {
        let st = windows::Win32::System::SystemInformation::GetSystemTime();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_init() {
        let temp = std::env::temp_dir();
        let sched = SchedulerManager::new(&temp, false);
        // Note: Task Scheduler COM service is available in interactive Windows
        if let Ok(s) = sched {
            println!("Task Scheduler service connected: {}", s.task_name);
        }
    }
}
