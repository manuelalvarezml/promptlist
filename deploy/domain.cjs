// Provision only the dedicated domain stack after external DNS validates ACM.
const cp=require('node:child_process'),fs=require('node:fs');
const profile='manueldev',region='us-east-2',path='.deploy/domain-state.json';
function aws(args){return cp.execFileSync('aws',[...args,'--profile',profile,'--region',region,'--output','json'],{encoding:'utf8',stdio:['ignore','pipe','pipe']});}
const operator=JSON.parse(fs.readFileSync('.deploy/operator.json','utf8'));
const identity=JSON.parse(aws(['sts','get-caller-identity']));
if(identity.Account!==operator.account||identity.Arn!==operator.arn)throw Error('Wrong AWS identity.');
const state=JSON.parse(fs.readFileSync(path,'utf8'));
if(state.domain!=='promptlist.lat'||state.stack!=='promptlist-domain')throw Error('Unexpected domain scope.');
const cert=JSON.parse(aws(['acm','describe-certificate','--certificate-arn',state.certificateArn])).Certificate;
if(cert.DomainName!==state.domain||!cert.SubjectAlternativeNames.includes('www.'+state.domain))throw Error('Certificate domain mismatch.');
if(cert.Status!=='ISSUED'){console.log('Certificate still needs DNS validation. No domain stack changed.');process.exit(2);}
aws(['cloudformation','deploy','--template-file','deploy/domain.json','--stack-name',state.stack,'--parameter-overrides','Domain='+state.domain,'CertificateArn='+state.certificateArn,'FunctionName=promptlist-api','--tags','Project=promptlist','--no-fail-on-empty-changeset']);
const stack=JSON.parse(aws(['cloudformation','describe-stacks','--stack-name',state.stack])).Stacks[0];
state.outputs=Object.fromEntries(stack.Outputs.map(o=>[o.OutputKey,o.OutputValue]));state.status=stack.StackStatus;
fs.writeFileSync(path,JSON.stringify(state,null,2),{mode:0o600});fs.chmodSync(path,0o600);
console.log(JSON.stringify({status:state.status,dns:state.outputs}));
