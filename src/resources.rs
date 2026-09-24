use anyhow::Result;
use serde_json::Value;
use tokio::process::Command;

pub async fn sample() -> Result<Value> {
    let mut cmd = Command::new("python3");
    cmd.args(["-c",r#"
import json,time,shutil,os
def cpu():
    x=list(map(int,open('/proc/stat').readline().split()[1:9]))
    return sum(x),x[3]+x[4]
a=cpu();time.sleep(.25);b=cpu()
mem={p[0].rstrip(':'):int(p[1])*1024 for p in (l.split() for l in open('/proc/meminfo')) if len(p)>1 and p[1].isdigit()}
d=shutil.disk_usage('/workspace')
print(json.dumps({'sampled_at':int(time.time()),'cpu_percent':round(100*(1-(b[1]-a[1])/max(1,b[0]-a[0])),1),'cpus':os.cpu_count(),'load_average':os.getloadavg(),'memory_total':mem['MemTotal'],'memory_used':mem['MemTotal']-mem.get('MemAvailable',mem.get('MemFree',0)),'swap_total':mem.get('SwapTotal',0),'swap_used':mem.get('SwapTotal',0)-mem.get('SwapFree',0),'disk_total':d.total,'disk_used':d.used,'disk_free':d.free,'uptime_seconds':int(float(open('/proc/uptime').read().split()[0]))}))
"#]);
    let bytes = crate::vm::capture(cmd, None, 5, 8192).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
