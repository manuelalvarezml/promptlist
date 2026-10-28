// Operator command. Secrets travel only in mode-0600 CLI input, never argv/logs.
const fs=require('node:fs'),cp=require('node:child_process'),crypto=require('node:crypto');
const {parseEnv}=require('node:util');
const profile='manueldev',region='us-east-2',stack='promptlist-serverless';
const dir='.deploy',receipt=dir+'/aws-release.json';
fs.mkdirSync(dir,{recursive:true,mode:0o700});fs.chmodSync(dir,0o700);
function aws(args){return cp.execFileSync('aws',[...args,'--profile',profile,'--region',region,'--output','json'],{encoding:'utf8',stdio:['ignore','pipe','pipe'],maxBuffer:8*1024*1024});}
function call(args,input){const file=dir+'/private-request.json';fs.writeFileSync(file,JSON.stringify(input),{mode:0o600});fs.chmodSync(file,0o600);try{return aws([...args,'--cli-input-json','file://'+file]);}finally{fs.unlinkSync(file);}}
function save(state){fs.writeFileSync(receipt,JSON.stringify(state,null,2),{mode:0o600});}
const identity=JSON.parse(aws(['sts','get-caller-identity']));
const operator=JSON.parse(fs.readFileSync(dir+'/operator.json','utf8'));
if(identity.Account!==operator.account||identity.Arn!==operator.arn)throw Error('Wrong AWS identity; no deployment attempted.');
const env=parseEnv(fs.readFileSync('.env.local','utf8'));
const accounts=cp.execFileSync('sqlite3',['-readonly','.data/catalog.sqlite',"SELECT DISTINCT account FROM records WHERE kind='snapshot';"],{encoding:'utf8'}).trim().split('\n').filter(Boolean);
if(!accounts.length||accounts.length>5)throw Error('Expected a bounded local owner account list.');
let state=fs.existsSync(receipt)?JSON.parse(fs.readFileSync(receipt,'utf8')):{profile,region,stack,releaseBucket:'promptlist-releases-'+identity.Account+'-'+crypto.randomBytes(4).toString('hex')};
save(state);
const mode=process.argv[2]||'deploy';
if(!state.bucketReady){
  // A recorded name is reused after unknown outcomes; never create replacement buckets.
  try{aws(['s3api','head-bucket','--bucket',state.releaseBucket]);}
  catch(e){if(!String(e.stderr).includes('404'))throw Error('Release bucket status unknown; inspect before retrying.');aws(['s3api','create-bucket','--bucket',state.releaseBucket,'--create-bucket-configuration','LocationConstraint='+region]);}
  aws(['s3api','put-public-access-block','--bucket',state.releaseBucket,'--public-access-block-configuration','BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true']);
  call(['s3api','put-bucket-encryption'],{Bucket:state.releaseBucket,ServerSideEncryptionConfiguration:{Rules:[{ApplyServerSideEncryptionByDefault:{SSEAlgorithm:'AES256'}}]}});
  call(['s3api','put-bucket-lifecycle-configuration'],{Bucket:state.releaseBucket,LifecycleConfiguration:{Rules:[{ID:'bounded-releases',Status:'Enabled',Filter:{Prefix:'releases/'},Expiration:{Days:30},AbortIncompleteMultipartUpload:{DaysAfterInitiation:1}}]}});
  state.bucketReady=true;save(state);
}
if(mode==='deploy'){
  const release=dir+'/promptlist-lambda.zip';
  if(!fs.existsSync(dir+'/lambda/bootstrap'))throw Error('Build the Lambda artifact first.');
  fs.copyFileSync(dir+'/lambda/bootstrap',dir+'/bootstrap');fs.chmodSync(dir+'/bootstrap',0o755);
  cp.execFileSync('zip',['-q','-j',release,dir+'/bootstrap']);
  const assets=['assets/island-background.jpg',...fs.readdirSync('assets/fonts').filter(n=>n.endsWith('.woff2')).map(n=>'assets/fonts/'+n)];
  cp.execFileSync('zip',['-q',release,...assets]);
  const digest=crypto.createHash('sha256').update(fs.readFileSync(release)).digest('hex');
  state.releaseKey='releases/'+digest+'.zip';state.binaryHash=crypto.createHash('sha256').update(fs.readFileSync(dir+'/lambda/bootstrap')).digest('hex');save(state);
  aws(['s3api','put-object','--bucket',state.releaseBucket,'--key',state.releaseKey,'--body',release,'--server-side-encryption','AES256']);
}
const parameters={ReleaseBucket:state.releaseBucket,ReleaseKey:state.releaseKey,SpotifyClientId:env.SPOTIFY_CLIENT_ID,OpenAIKey:env.OPENAI_API_KEY,LastfmKey:env.LASTFM_API_KEY,AllowedUsers:[...accounts,...(mode==='finalize'?[]:['promptlist-cloud-smoke'])].join(','),Callback:state.url?state.url+'callback':'https://pending.invalid/callback'};
const input={StackName:stack,TemplateBody:fs.readFileSync('deploy/serverless.json','utf8'),Parameters:Object.entries(parameters).map(([ParameterKey,ParameterValue])=>({ParameterKey,ParameterValue})),Capabilities:['CAPABILITY_IAM'],Tags:[{Key:'Project',Value:'promptlist'}]};
let exists=false;try{aws(['cloudformation','describe-stacks','--stack-name',stack]);exists=true;}catch(e){if(!String(e.stderr).includes('does not exist'))throw Error('Stack status unknown.');}
try{call(['cloudformation',exists?'update-stack':'create-stack'],input);console.log('Promptlist stack '+(exists?'update':'creation')+' started.');aws(['cloudformation','wait',exists?'stack-update-complete':'stack-create-complete','--stack-name',stack]);}
catch(e){if(!String(e.stderr).includes('No updates are to be performed')){console.error('Deployment not confirmed. Inspect this stack events; secrets were not printed.');process.exit(1);}}
const result=JSON.parse(aws(['cloudformation','describe-stacks','--stack-name',stack])).Stacks[0];
state.outputs=Object.fromEntries(result.Outputs.map(v=>[v.OutputKey,v.OutputValue]));state.url=state.outputs.Url;state.status=result.StackStatus;save(state);
console.log(JSON.stringify({status:state.status,url:state.url,needsCallbackUpdate:parameters.Callback!==state.url+'callback'}));
