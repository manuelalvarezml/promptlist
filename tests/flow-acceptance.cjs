// Synthetic fixture only; never logs into a real provider or syncs a real library.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert=require('node:assert/strict');
(async()=>{
  const browser=await chromium.launch({headless:true,channel:'chrome'});
  try {
    const context=await browser.newContext();
    const page=await context.newPage();
    for(const width of [1440,1024,390,320]) {
      await page.setViewportSize({width,height:900});
      await page.goto('http://127.0.0.1:8788/');
      const button=page.getByRole('button',{name:'Connect Spotify',exact:true});
      const box=await button.boundingBox();
      assert(box && box.width>200 && box.x>=0 && box.x+box.width<=width);
      assert.equal(await page.locator('nav,#prompt,#radio-player').count(),0);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
      await page.screenshot({path:`/tmp/promptlist-connect-${width}.png`});
    }
    await context.addCookies([{name:'promptlist_session',value:'fixture',url:'http://127.0.0.1:8788',httpOnly:true,sameSite:'Lax'}]);
    for(const width of [1440,1024,390,320]) {
      await page.setViewportSize({width,height:900});
      await page.goto('http://127.0.0.1:8788/?tab=library&ui=web');
      assert.equal(await page.locator('.track-details,.diagnostic-stats,main h1').count(),0);
      assert.equal(await page.getByRole('link',{name:'Coverage',exact:true}).count(),0);
      const navY=await page.locator('main > nav').evaluate(el=>el.getBoundingClientRect().y+scrollY);
      if(width>=1024) {
        assert((await page.locator('.table-wrap').boundingBox()).height>450);
        assert.equal(await page.evaluate(()=>document.documentElement.scrollHeight<=innerHeight),true);
      }
      await page.getByRole('link',{name:'Sources',exact:true}).click();
      await page.getByRole('region',{name:'Library sources',exact:true}).waitFor();
      assert.equal(await page.locator('main > nav').evaluate(el=>el.getBoundingClientRect().y+scrollY),navY);
      assert.equal(await page.locator('main h1,main h2').count(),0);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
      await page.screenshot({path:`/tmp/promptlist-sources-web-${width}.png`});
      await page.getByRole('link',{name:'Local',exact:true}).click();
      await page.getByRole('region',{name:'Local diagnostics',exact:true}).waitFor();
      assert.equal(await page.getByRole('link',{name:'Coverage',exact:true}).count(),1);
      assert.equal(await page.getByRole('columnheader',{name:'Library tracks',exact:true}).count(),1);
      await page.getByRole('link',{name:'Tracks',exact:true}).click();
      await page.locator('.track-details').first().waitFor();
      assert.equal(await page.locator('main > nav').evaluate(el=>el.getBoundingClientRect().y+scrollY),navY);
      await page.screenshot({path:`/tmp/promptlist-library-local-${width}.png`});
      await page.goto('http://127.0.0.1:8788/?ui=web');
      assert.equal(await page.locator('#prompt').evaluate(el=>getComputedStyle(el).resize),'none');
    }
    // A connection-marked page submits one automatic sync, never on polling.
    let posts=0;
    await page.route('**/sync',async route=>{
      posts++;
      await route.fulfill({status:200,contentType:'text/html',body:'<html><head><title>Sync accepted</title></head><body><header></header><main><section id="job-status" data-running="false" hidden></section></main><footer></footer></body></html>'});
    });
    await page.route('**/?auto-fixture=1',async route=>{
      const response=await route.fetch();
      let html=(await response.text()).replace('</main>','<form id="auto-sync" action="/sync" method="post"><input name="consent" value="automatic" type="hidden"></form></main>');
      if(posts) html=html.replace(/<title>[^<]*<\/title>/,'<title>Sync accepted</title>');
      await route.fulfill({response,body:html});
    });
    await page.goto('http://127.0.0.1:8788/?auto-fixture=1');
    await page.waitForFunction(()=>document.title==='Sync accepted');
    assert.equal(posts,1);
    console.log('PASS: centered connect gate, Web/Local switch, hidden diagnostics, merged library, stable navigation, large desktop tables, no text resizing, automatic sync transport; 1440/1024/390/320.');
  } finally { await browser.close(); }
})().catch(error=>{console.error(error);process.exitCode=1;});
