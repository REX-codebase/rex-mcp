import { useState } from "react";
import { ToolApproval, ToolReceiptCard } from "./ToolApproval";
import type { PreparedToolCall, ToolResult } from "../data/backend";
const call:PreparedToolCall={call_id:"preview",tool:"edit_file",summary:"Edit src/runtime.ts after matching the exact current block",risk:"write",approval_required:true,policy_reason:"Writes pause for an explicit trusted UI decision"};
const result:ToolResult={call_id:"preview",ok:true,tool:"edit_file",state:"executed",output:"Updated src/runtime.ts",error:null,receipt:{started_at_ms:0,duration_ms:47,target:"src/runtime.ts",command:null,exit_code:null,bytes_read:380,bytes_written:412,output_truncated:false,diff:"@@ agent loop\n- const limit = Infinity\n+ const limit = 12",redactions:0}};
export function ToolApprovalPreview(){const [done,setDone]=useState(false);return <div>{done?<ToolReceiptCard result={result}/>:<ToolApproval call={call} busy={false} onDecision={ok=>ok&&setDone(true)}/>}</div>}
