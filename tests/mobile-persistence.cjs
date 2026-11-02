// Synthetic fixture only. No Spotify login, model call or real export.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert=require('node:assert/strict');
(async()=>{
  const browser=await chromium.launch({headless:true,channel:'chrome'});
  try {
    const context=await browser.newContext({viewport:{width:390,height:850},isMobile:true,hasTouch:true});
    await context.addCookies([{name:'promptlist_session',value:'fixture',url:'http://127.0.0.1:8788',httpOnly:true,sameSite:'Lax'}]);
    const page=await context.newPage();
    const errors=[]; page.on('pageerror',e=>errors.push(e.message));
    const response=await page.goto('http://127.0.0.1:8788/');
    assert.match((await response.allHeaders())['set-cookie'],/Max-Age=7776000/);
    await page.locator('#prompt').fill('8 tracks');
    await page.getByRole('button',{name:'Build preview',exact:true}).click();
    await page.waitForFunction(()=>document.querySelectorAll('.track-row').length===8);
    assert((await page.locator('.save-form input[name=name]').inputValue()).split(/\s+/).length<=4);
    assert.equal(await page.locator('.composer-form input[name=name]').count(),0);
    for(const width of [320,390,1024,1440]) {
      const viewportContext=await browser.newContext({viewport:{width,height:850},isMobile:width<1024,hasTouch:width<1024});
      await viewportContext.addCookies(await context.cookies());
      const viewportPage=await viewportContext.newPage();
      await viewportPage.goto('http://127.0.0.1:8788/');
      await viewportPage.evaluate(async()=>{await document.fonts.ready;await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));});
      const metrics=await viewportPage.evaluate(()=>({
        playerBottom:document.querySelector('#radio-player').getBoundingClientRect().bottom,
        playerPosition:getComputedStyle(document.querySelector('#radio-player')).position,
        headerImage:getComputedStyle(document.querySelector('body > header')).backgroundImage,
        backgroundPosition:getComputedStyle(document.body,'::before').position,
        overflow:document.documentElement.scrollWidth>innerWidth,
        shadow:getComputedStyle(document.querySelector('#prompt')).boxShadow
      }));
      assert.equal(metrics.headerImage,'none'); assert.equal(metrics.backgroundPosition,'fixed');
      assert.equal(metrics.overflow,false); assert.equal(metrics.shadow,'none');
      if(width<1024) {assert.equal(metrics.playerPosition,'fixed');assert(metrics.playerBottom<=851 && metrics.playerBottom>=849);}
      assert.equal(await viewportPage.locator('body > header').count(),1);
      if(width>=1024) {
        const exportBox=await viewportPage.getByRole('button',{name:'Export to Spotify',exact:true}).boundingBox();
        assert(exportBox.y+exportBox.height<=850,'desktop export must fit the viewport');
      }
      await viewportPage.screenshot({path:`/tmp/promptlist-mobile-release-${width}.png`});
      await viewportContext.close();
    }
    await page.setViewportSize({width:390,height:850});
    let edits=0;
    await page.route('**/preview/edit',async route=>{
      edits++;
      await new Promise(resolve=>setTimeout(resolve,800));
      await route.continue();
    });
    const before=await page.locator('#selection').getAttribute('data-preview-id');
    for(let i=0;i<3;i++) await page.getByRole('button',{name:'Remove track',exact:true}).first().click();
    assert.equal(await page.locator('.track-row').count(),5,'removals must appear before network completes');
    assert(await page.getByRole('button',{name:'Export to Spotify',exact:true}).isDisabled());
    await page.waitForFunction(()=>document.querySelector('.preview').getAttribute('aria-busy')==='false');
    assert.notEqual(await page.locator('#selection').getAttribute('data-preview-id'),before);
    assert(edits<=2,'rapid clicks should coalesce');
    await page.reload(); assert.equal(await page.locator('.track-row').count(),5);
    await page.unroute('**/preview/edit');
    let failures=0;
    await page.route('**/preview/edit',async route=>{failures++;await route.abort();});
    await page.getByRole('button',{name:'Remove track',exact:true}).first().click();
    await page.waitForFunction(()=>document.querySelectorAll('.track-row').length===5 && document.querySelector('.preview').getAttribute('aria-busy')==='false');
    assert.equal(failures,1,'ambiguous edits must not retry');
    await page.unroute('**/preview/edit');
    await page.locator('#prompt').fill('Keep this unfinished prompt');
    await page.evaluate(()=>{
      document.querySelector('.build-button').disabled=true;
      window.dispatchEvent(new PageTransitionEvent('pageshow',{persisted:true}));
    });
    await page.waitForFunction(()=>!document.querySelector('.build-button').disabled);
    assert.equal(await page.locator('#prompt').inputValue(),'Keep this unfinished prompt');
    assert.deepEqual(errors,[]);
    console.log('PASS: 90-day cookie, compact naming, bottom mobile player, one fixed background, instant batched removal, export lock, failure reconciliation, BFCache recovery and draft preservation; 320/390/1024/1440.');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
