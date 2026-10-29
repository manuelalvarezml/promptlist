const cp=require('node:child_process'),fs=require('node:fs');
const args=process.argv.slice(2),state=JSON.parse(fs.readFileSync('.deploy/aws-release.json','utf8'));
const identity=JSON.parse(cp.execFileSync('aws',['sts','get-caller-identity','--profile','manueldev','--region','us-east-2'],{encoding:'utf8'}));
const operator=JSON.parse(fs.readFileSync('.deploy/operator.json','utf8'));
if(identity.Account!==operator.account||identity.Arn!==operator.arn)throw Error('Wrong AWS identity.');
const c=JSON.parse(cp.execFileSync('aws',['configure','export-credentials','--profile','manueldev','--format','process'],{encoding:'utf8'}));
const env={...process.env,AWS_ACCESS_KEY_ID:c.AccessKeyId,AWS_SECRET_ACCESS_KEY:c.SecretAccessKey,AWS_REGION:'us-east-2',PROMPTLIST_TABLE:state.outputs.StateTable,PROMPTLIST_BUCKET:state.outputs.DataBucket,PROMPTLIST_CLOUD_URL:state.publicUrl||state.url};
if(c.SessionToken)env.AWS_SESSION_TOKEN=c.SessionToken;else delete env.AWS_SESSION_TOKEN;
let command='cargo',commandArgs=['run','--quiet','--bin',...args];
if(args[0]==='--container'){
  args.shift();
  if(!['cloud-smoke','cloud-import'].includes(args[0]))throw Error('Unsupported operator binary.');
  command='docker';
  commandArgs=['run','--rm','--platform','linux/amd64','--read-only',
    '--mount',`type=bind,source=${process.cwd()},target=/work,readonly`,
    '--mount',`type=bind,source=${process.cwd()}/.deploy,target=/work/.deploy`,
    '--workdir','/work',
    ...['AWS_ACCESS_KEY_ID','AWS_SECRET_ACCESS_KEY','AWS_SESSION_TOKEN','AWS_REGION','PROMPTLIST_TABLE','PROMPTLIST_BUCKET','PROMPTLIST_CLOUD_URL'].filter(k=>env[k]).flatMap(k=>['--env',k]),
    'rust:1.89-alpine',`/work/.deploy/lambda/${args[0]}`,...args.slice(1).filter(v=>v!=='--')];
}
const result=cp.spawnSync(command,commandArgs,{env,stdio:'inherit'});
process.exit(result.status??1);
