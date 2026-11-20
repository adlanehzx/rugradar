// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// Plain ERC-20: should be reported as safe.
contract NormalToken is ERC20 {
    constructor() ERC20("Normal", "NORM") {
        _mint(msg.sender, 1_000_000 ether);
    }
}

/// Takes `taxBps` on every transfer that touches the pair (buys and sells) and `transferTaxBps`
/// on wallet-to-wallet transfers.
contract TaxToken is ERC20 {
    address public pair;
    address public immutable owner;
    uint256 public immutable taxBps;
    uint256 public immutable transferTaxBps;

    constructor(uint256 taxBps_, uint256 transferTaxBps_) ERC20("Taxed", "TAX") {
        owner = msg.sender;
        taxBps = taxBps_;
        transferTaxBps = transferTaxBps_;
        _mint(msg.sender, 1_000_000 ether);
    }

    function setPair(address pair_) external {
        require(msg.sender == owner, "owner");
        pair = pair_;
    }

    function _update(address from, address to, uint256 amount) internal override {
        if (from == owner || to == owner || pair == address(0) || from == address(0)) {
            super._update(from, to, amount);
            return;
        }
        uint256 bps = (from == pair || to == pair) ? taxBps : transferTaxBps;
        uint256 fee = (amount * bps) / 10_000;
        if (fee > 0) super._update(from, owner, fee);
        super._update(from, to, amount - fee);
    }
}

/// Anyone can buy, nobody except the owner can sell back to the pair.
contract HoneypotToken is ERC20 {
    address public pair;
    address public immutable owner;

    constructor() ERC20("Honey", "HONEY") {
        owner = msg.sender;
        _mint(msg.sender, 1_000_000 ether);
    }

    function setPair(address pair_) external {
        require(msg.sender == owner, "owner");
        pair = pair_;
    }

    function _update(address from, address to, uint256 amount) internal override {
        if (to == pair && from != owner && pair != address(0)) revert("sells disabled");
        super._update(from, to, amount);
    }
}

contract FalseReturnToken is ERC20 {
    uint8 public mode;
    constructor(uint8 m) ERC20("False return", "FALSE") { mode=m; _mint(msg.sender,1_000_000 ether); }
    function transfer(address to,uint256 value) public override returns(bool) {
        if(msg.sender==address(0x5151aa) && mode==1) return false;
        return super.transfer(to,value);
    }
    function approve(address spender,uint256 value) public override returns(bool) {
        if(msg.sender==address(0x5151aa) && mode==2) return false;
        return super.approve(spender,value);
    }
}
